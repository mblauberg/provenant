//! Document-based graph service shared by the CLI and MCP transports.
use super::*;
use provenant_fabric_runtime::graph::{
    FileJournal, GraphDocument, GraphJournal, GraphStatus, Layer, Template, TemplateLibrary,
    patch::{self, LiveGraph, Patch, PatchContext},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn encode(value: impl serde::Serialize) -> Result<Value, FacadeError> {
    serde_json::to_value(value).map_err(|_| FacadeError::StateCorrupt)
}

fn required<'a>(input: &'a Value, field: &str) -> Result<&'a str, FacadeError> {
    input[field]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| FacadeError::invalid(format!("{field} is required")))
}

impl Factory {
    fn launch_resolvers(
        &self,
        document: Option<&Value>,
    ) -> Result<BTreeMap<String, Vec<String>>, FacadeError> {
        let config = provenant_config::read_with_domains(
            &self.context.roots,
            &provenant_cli::domains::SECTIONS,
        )
        .map_err(|error| FacadeError::invalid(error.to_string()))?;
        let mut request: provenant_routing::ReviewRequest = document
            .and_then(|document| document.get("review"))
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| {
                FacadeError::invalid(format!("invalid graph review request: {error}"))
            })?
            .unwrap_or_default();
        if let Some(context) = document.and_then(|document| document.get("context")) {
            request.risk_tier = request
                .risk_tier
                .or_else(|| context["risk_tier"].as_str().map(str::to_owned));
            request.kind = request
                .kind
                .or_else(|| context["task_kind"].as_str().map(str::to_owned));
        }
        let panel = config
            .domains
            .routing
            .review
            .resolve_panel(&request, None, &[], false)
            .map_err(FacadeError::invalid)?;
        Ok(BTreeMap::from([(
            "review-panel".into(),
            panel.slots.into_iter().map(|slot| slot.lens).collect(),
        )]))
    }

    fn relay_document(
        &self,
        input: &Value,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Value, FacadeError> {
        if input.get("document").is_some_and(|value| !value.is_null())
            || input.get("over").is_some_and(|value| !value.is_null())
            || input
                .get("set")
                .is_some_and(|value| value.as_object().is_none_or(|object| !object.is_empty()))
        {
            return Err(FacadeError::invalid(
                "the lane relay accepts a template, brief and tier; document, over and set require host submission",
            ));
        }
        // Distinct submissions may have identical briefs. Generate the key once
        // per call; transport retries retain this request and its identity.
        let key = format!(
            "child-{}",
            provenant_core::hex(
                &provenant_os::fs::entropy_128()
                    .map_err(|_| FacadeError::io("create child operation identity"))?
            )
        );
        let request = provenant_fabric_runtime::graph::relay::ChildRequest {
            id: key.clone(),
            template: input
                .get("use")
                .and_then(Value::as_str)
                .unwrap_or("task")
                .into(),
            brief: required(input, "brief")?.into(),
            tier: input.get("tier").and_then(Value::as_str).map(str::to_owned),
            timeout_seconds: None,
            writer: false,
            write_scope: vec![],
            detached: false,
            idempotency_key: key,
            reason: Some("run".into()),
        };
        if input["explain"] == true {
            return encode(request);
        }
        let within =
            std::time::Duration::from_micros(budget.remaining(self.clock.mono_now()).as_micros());
        let receipt = provenant_cli::fabric::graph_relay::from_lane(
            &provenant_fabric_runtime::graph::relay::RelayOp::Add(request),
            cancel,
            within,
        )
        .map_err(|error| FacadeError::invalid(error.to_string()))?;
        if let Some(seconds) = input.get("wait_seconds").and_then(Value::as_u64) {
            let children = provenant_cli::fabric::graph_relay::wait_for_children(
                cancel,
                std::time::Duration::from_secs(seconds),
            )
            .map_err(|error| FacadeError::invalid(error.to_string()))?;
            return Ok(json!({"receipt":receipt,"children":children}));
        }
        Ok(receipt)
    }

    fn confine_node(&self, node: &mut Value) -> Result<(), FacadeError> {
        let policy = provenant_fabric_runtime::read_roots::ReadRoots::new(
            crate::preset::project_worktrees(&self.cwd),
            &self.context.config.graph.allowed_read_roots,
        );
        crate::preset::confine_pattern_roots(node, &policy).map_err(FacadeError::invalid)?;
        if let Some(roots) = node.get_mut("read_roots").and_then(Value::as_array_mut) {
            for root in roots {
                let given = root
                    .as_str()
                    .ok_or_else(|| FacadeError::invalid("read_roots entries must be strings"))?;
                let path = Path::new(given);
                let path = if path.is_absolute() {
                    path.to_owned()
                } else {
                    self.cwd.join(path)
                };
                *root = encode(policy.confine(&path).map_err(FacadeError::invalid)?)?;
            }
        }
        for pointer in ["/composite/nodes", "/loop/body", "/plan/fallback/nodes"] {
            if let Some(nodes) = node.pointer_mut(pointer).and_then(Value::as_array_mut) {
                for node in nodes {
                    self.confine_node(node)?;
                }
            }
        }
        Ok(())
    }

    fn confine_nodes(&self, document: &mut Value) -> Result<(), FacadeError> {
        if let Some(nodes) = document.get_mut("nodes").and_then(Value::as_array_mut) {
            for node in nodes {
                self.confine_node(node)?;
            }
        }
        Ok(())
    }

    fn template_library(&self, inline: Option<&Value>) -> Result<TemplateLibrary, FacadeError> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| self.home.join(".config"));
        if !config.is_absolute() {
            return Err(FacadeError::invalid("XDG_CONFIG_HOME must be absolute"));
        }
        let mut library = TemplateLibrary::bundled();
        library
            .load_dir(Layer::User, &config.join("provenant/templates"))
            .map_err(FacadeError::invalid)?;
        library
            .load_dir(Layer::Repository, &self.cwd.join(".provenant/templates"))
            .map_err(FacadeError::invalid)?;
        if let Some(inline) = inline.filter(|value| !value.is_null()) {
            let templates: BTreeMap<String, Template> = serde_json::from_value(inline.clone())
                .map_err(|error| FacadeError::invalid(format!("inline templates: {error}")))?;
            library = library
                .with(Layer::Inline, &templates)
                .map_err(FacadeError::invalid)?;
        }
        Ok(library)
    }

    /// Both transports pass documents and return the same JSON result.
    pub(crate) fn graph_document(
        &self,
        input: Value,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Value, FacadeError> {
        let operation = required(&input, "op")?;
        if let Some(wait) = input.get("wait_seconds") {
            let seconds = wait
                .as_u64()
                .ok_or_else(|| FacadeError::invalid("wait_seconds must be an integer"))?;
            WaitBudget::cli(self.clock.mono_now(), seconds)
                .map_err(|error| FacadeError::invalid(error.to_string()))?;
        }
        if self.context.inspection_only
            && (matches!(operation, "edit" | "stop" | "retry")
                || operation == "run" && input["explain"] != true)
        {
            return Err(FacadeError::ConfinementRefused);
        }
        if self.context.in_lane {
            if operation == "run" {
                return self.relay_document(&input, budget, cancel);
            }
            if matches!(operation, "edit" | "stop" | "retry") {
                return Err(FacadeError::ConfinementRefused);
            }
        }
        match operation {
            "templates" => {
                let library = self.template_library(input.get("templates"))?;
                let names: Vec<_> = match input.get("name").and_then(Value::as_str) {
                    Some(name) => vec![name],
                    None => library.names().collect(),
                };
                let templates = names
                    .into_iter()
                    .map(|name| {
                        Ok(json!({
                            "name":name,
                            "layer":library.layer(name),
                            "template":library.resolve(name).map_err(FacadeError::invalid)?,
                        }))
                    })
                    .collect::<Result<Vec<_>, FacadeError>>()?;
                Ok(json!({"templates":templates}))
            }
            "run" => self.submit_document(&input, budget, cancel),
            "edit" => self.edit_document(&input, budget, cancel),
            "trace" => self.graph_trace(required(&input, "id")?),
            "output" if input["trace"] == true => self.graph_trace(required(&input, "id")?),
            "status" if input["stats"] == true => self.graph_stats(&input),
            "status" => {
                let target = self
                    .graph_target(required(&input, "id")?)?
                    .ok_or(FacadeError::NotFound)?;
                let run = self.graph(
                    GraphInput::Status {
                        extra: Default::default(),
                        id: target.graph.clone(),
                    },
                    budget,
                    cancel,
                )?;
                self.control_receipt(run, &target, &input, budget, cancel, false)
            }
            "stop" => {
                let target = self
                    .graph_target(required(&input, "id")?)?
                    .ok_or(FacadeError::NotFound)?;
                let run = self.stop_target(
                    &target,
                    input
                        .get("reason")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    budget,
                    cancel,
                )?;
                self.control_receipt(run, &target, &input, budget, cancel, true)
            }
            "retry" => self.retry_document(&input, budget, cancel),
            _ => Err(FacadeError::invalid("unknown graph operation")),
        }
    }

    fn submit_document(
        &self,
        input: &Value,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Value, FacadeError> {
        let raw = input.get("document");
        if raw.is_some_and(|document| !document.is_object()) {
            return Err(FacadeError::invalid(
                "document must be an object, not a file path",
            ));
        }
        if raw.is_some()
            && (["brief", "use", "tier", "over"]
                .iter()
                .any(|field| input.get(*field).is_some_and(|value| !value.is_null()))
                || input
                    .get("set")
                    .is_some_and(|value| value.as_object().is_none_or(|object| !object.is_empty())))
        {
            return Err(FacadeError::invalid(
                "a document cannot be combined with brief, use, tier, over or set overrides",
            ));
        }
        let library = self.template_library(raw.and_then(|document| document.get("templates")))?;
        let resolver = self.launch_resolvers(raw)?;
        let context = PatchContext {
            library: &library,
            resolvers: &resolver,
            scan: &patch::scan_for_secrets,
            journal: None,
            limits: &self.context.config.graph.limits,
        };
        let mut identity = input.clone();
        if let Some(fields) = identity.as_object_mut() {
            fields.remove("explain");
            fields.remove("wait_seconds");
        }
        let digest =
            provenant_records::CanonicalV1::record_digest("graph-submission", 1, &identity)
                .map_err(|_| FacadeError::StateCorrupt)?;
        let generated_id = format!("g-{}", &provenant_core::hex(digest.as_bytes())[..30]);
        let id = raw
            .and_then(|document| document.get("graph_id"))
            .and_then(Value::as_str)
            .unwrap_or(&generated_id);
        let mut document = if let Some(raw) = raw {
            if raw["nodes"]
                .as_array()
                .is_some_and(|nodes| nodes.iter().any(|node| node.get("use").is_some()))
                || raw.get("templates").is_some()
            {
                let patch = Patch {
                    schema_version: patch::PATCH_SCHEMA_VERSION,
                    nodes: serde_json::from_value(raw["nodes"].clone())
                        .map_err(|error| FacadeError::invalid(error.to_string()))?,
                    ..Default::default()
                };
                let mut document = patch::submit(id, &patch, &context)
                    .map_err(|error| FacadeError::invalid(error.to_string()))?;
                // Graph-wide settings belong to the document rather than a node.
                let mut compiled = encode(&document)?;
                for key in [
                    "limits",
                    "integration_branch",
                    "delivery",
                    "review_base",
                    "context",
                    "decision_session",
                ] {
                    if let Some(value) = raw.get(key) {
                        compiled[key] = value.clone();
                    }
                }
                document = serde_json::from_value(compiled)
                    .map_err(|error| FacadeError::invalid(error.to_string()))?;
                document
            } else {
                serde_json::from_value::<GraphDocument>(raw.clone())
                    .map_err(|error| FacadeError::invalid(error.to_string()))?
            }
        } else {
            let mut node = json!({"id":"task", "use":input.get("use").and_then(Value::as_str).unwrap_or("task")});
            for field in ["brief", "tier", "over"] {
                if let Some(value) = input.get(field).filter(|value| !value.is_null()) {
                    node[field] = value.clone();
                }
            }
            let template = library
                .resolve(node["use"].as_str().unwrap_or("task"))
                .map_err(FacadeError::invalid)?;
            let mut parameters = input
                .get("set")
                .filter(|value| !value.is_null())
                .cloned()
                .unwrap_or_else(|| json!({}));
            if !parameters.is_object() {
                return Err(FacadeError::invalid("set must be a parameter object"));
            }
            if template.parameters.contains_key("brief")
                && let Some(brief) = input.get("brief").filter(|value| !value.is_null())
            {
                parameters["brief"] = brief.clone();
            }
            if template.is_composite() {
                node.as_object_mut().unwrap().remove("brief");
                node.as_object_mut().unwrap().remove("tier");
            }
            node["with"] = parameters;
            patch::submit(
                id,
                &Patch {
                    schema_version: patch::PATCH_SCHEMA_VERSION,
                    nodes: vec![node],
                    ..Default::default()
                },
                &context,
            )
            .map_err(|error| FacadeError::invalid(error.to_string()))?
        };
        if raw.is_none()
            && let Some(tier) = input.get("tier").filter(|value| !value.is_null())
        {
            let tier = tier
                .as_str()
                .ok_or_else(|| FacadeError::invalid("tier must name a route alias"))?;
            let route = serde_json::from_value(json!({"alias":tier}))
                .map_err(|_| FacadeError::StateCorrupt)?;
            for node in &mut document.nodes {
                override_tier(node, &route);
            }
        }
        let mut bound = encode(&document)?;
        self.confine_nodes(&mut bound)?;
        document = serde_json::from_value(bound)
            .map_err(|error| FacadeError::invalid(error.to_string()))?;
        let submission_key = format!("submit:{}", document.graph_id);
        patch::submission(
            &mut document,
            &submission_key,
            &self.context.config.graph.limits,
        )
        .map_err(|error| FacadeError::invalid(error.to_string()))?;
        if input["explain"] == true {
            return Ok(json!({"document":document}));
        }
        let scratch = self
            .prepare_scratch(cancel, budget)
            .map_err(scratch_prepare_error)?;
        let path = publish_preset_graph(&scratch, &encode(&document)?)
            .map_err(|error| FacadeError::invalid(error.to_string()))?;
        let run = self.graph_with_scratch(
            GraphInput::Run {
                extra: Default::default(),
                document: path,
            },
            budget,
            cancel,
            Some(&scratch),
        )?;
        let run = self.wait_document(run, input, budget, cancel)?;
        let timed_out = input.get("wait_seconds").and_then(Value::as_u64).is_some()
            && run.status == GraphStatus::Pending
            && !cancel.is_cancelled();
        let mut result = encode(run)?;
        result["timed_out"] = json!(timed_out);
        Ok(result)
    }

    fn edit_document(
        &self,
        input: &Value,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Value, FacadeError> {
        let id = self.resolve_graph(required(input, "id")?)?.0;
        let patch: Patch = serde_json::from_value(input["patch"].clone())
            .map_err(|error| FacadeError::invalid(error.to_string()))?;
        let mut journal = FileJournal::open(
            &self.context.roots.state,
            self.context.identity.project,
            &id,
            budget,
            cancel,
            &self.clock,
        )
        .map_err(graph_journal_error)?;
        let execution = FileJournal::execution(
            &self.context.roots.state,
            self.context.identity.project,
            &id,
        )
        .map_err(graph_journal_error)?
        .ok_or(FacadeError::NotFound)?;
        let mut checkpoint = journal
            .load(&id)
            .map_err(graph_journal_error)?
            .ok_or(FacadeError::NotFound)?;
        let library = self.template_library(None)?;
        let resolver = self.launch_resolvers(Some(&encode(&execution.document)?))?;
        let admitted = patch::admit_patch(
            &mut LiveGraph {
                document: &execution.document,
                checkpoint: &mut checkpoint,
            },
            None,
            &patch,
            &PatchContext {
                library: &library,
                resolvers: &resolver,
                scan: &patch::scan_for_secrets,
                journal: None,
                limits: &self.context.config.graph.limits,
            },
        )
        .map_err(|error| FacadeError::invalid(error.to_string()))?;
        for expansion in &mut checkpoint.expansions {
            for node in &mut expansion.nodes {
                let mut value = encode(&*node)?;
                self.confine_node(&mut value)?;
                // Expansion nodes carry computed topology outside their ordinary
                // serialisation. Bind only access fields, preserving that topology.
                bind_read_roots(node, &value)?;
            }
        }
        // save writes patch events before publishing the checkpoint, under this lock.
        journal.save(&checkpoint).map_err(graph_journal_error)?;
        Ok(
            json!({"graph_id":id,"revision":admitted.revision,"replayed":admitted.replayed,"added":admitted.added}),
        )
    }

    fn retry_document(
        &self,
        input: &Value,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Value, FacadeError> {
        let mut target = self
            .graph_target(required(input, "id")?)?
            .ok_or(FacadeError::NotFound)?;
        if let Some(node) = input.get("node").and_then(Value::as_str) {
            if target
                .node
                .as_deref()
                .is_some_and(|selected| selected != node)
            {
                return Err(FacadeError::invalid(
                    "node selector conflicts with node override",
                ));
            }
            target.node = Some(node.into());
        }
        let extend: Vec<String> = input
            .get("extend")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| FacadeError::invalid(error.to_string()))?
            .unwrap_or_default();
        if !extend.is_empty()
            && (target.node.is_some() || input.get("brief").is_some_and(|value| !value.is_null()))
        {
            return Err(FacadeError::invalid(
                "apply extend separately before redoing a node",
            ));
        }
        let id = target.graph.clone();
        if let Some(node) = target.node.as_deref() {
            let mut journal = FileJournal::open(
                &self.context.roots.state,
                self.context.identity.project,
                &id,
                budget,
                cancel,
                &self.clock,
            )
            .map_err(graph_journal_error)?;
            let execution = FileJournal::execution(
                &self.context.roots.state,
                self.context.identity.project,
                &id,
            )
            .map_err(graph_journal_error)?
            .ok_or(FacadeError::NotFound)?;
            let mut checkpoint = journal
                .load(&id)
                .map_err(graph_journal_error)?
                .ok_or(FacadeError::NotFound)?;
            let library = self.template_library(None)?;
            let resolver = self.launch_resolvers(Some(&encode(&execution.document)?))?;
            patch::retry_node(
                &mut LiveGraph {
                    document: &execution.document,
                    checkpoint: &mut checkpoint,
                },
                node,
                input.get("brief").and_then(Value::as_str),
                &PatchContext {
                    library: &library,
                    resolvers: &resolver,
                    scan: &patch::scan_for_secrets,
                    journal: None,
                    limits: &self.context.config.graph.limits,
                },
            )
            .map_err(|error| FacadeError::invalid(error.to_string()))?;
            journal.save(&checkpoint).map_err(graph_journal_error)?;
        } else if input.get("brief").is_some_and(|value| !value.is_null()) {
            return Err(FacadeError::invalid("brief requires a node target"));
        }
        let run = self.graph(
            GraphInput::Resume {
                extra: Default::default(),
                document: id.into(),
                extend,
            },
            budget,
            cancel,
        )?;
        target.attempt = None;
        self.control_receipt(run, &target, input, budget, cancel, false)
    }

    fn control_receipt(
        &self,
        mut run: provenant_fabric_runtime::graph::GraphRun,
        target: &super::unified::GraphTarget,
        input: &Value,
        budget: WaitBudget,
        cancel: &CancelToken,
        stopping: bool,
    ) -> Result<Value, FacadeError> {
        let mut timed_out = false;
        if let Some(seconds) = input.get("wait_seconds").and_then(Value::as_u64) {
            let wait = WaitBudget::cli(self.clock.mono_now(), seconds)
                .map_err(|error| FacadeError::invalid(error.to_string()))?
                .cap_at(budget.deadline().as_mono_time());
            loop {
                let unfinished = if stopping {
                    run.checkpoint
                        .nodes
                        .iter()
                        .filter(|node| {
                            target.node.as_ref().is_none_or(|id| {
                                &node.id == id
                                    || node.extra.get("subtree_cancel").and_then(Value::as_str)
                                        == Some(id)
                            })
                        })
                        .flat_map(|node| &node.attempts)
                        .any(|attempt| {
                            attempt.state != provenant_fabric_runtime::graph::AttemptState::Terminal
                        })
                } else {
                    run.status == GraphStatus::Pending
                };
                if !unfinished {
                    break;
                }
                let remaining = wait.remaining(self.clock.mono_now());
                if remaining.as_micros() == 0 || cancel.is_cancelled() {
                    timed_out = true;
                    break;
                }
                provenant_core::Pause::wait(
                    &self.backoff,
                    provenant_core::DurationMicros::from_micros(250_000.min(remaining.as_micros())),
                    cancel,
                );
                run = if stopping && target.node.is_some() {
                    self.stop_target(
                        target,
                        input
                            .get("reason")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        budget,
                        cancel,
                    )?
                } else {
                    self.graph(
                        GraphInput::Status {
                            extra: Default::default(),
                            id: target.graph.clone(),
                        },
                        budget,
                        cancel,
                    )?
                };
            }
        }
        Ok(json!({"rows":super::unified::status_rows(&run,target),"timed_out":timed_out}))
    }

    fn wait_document(
        &self,
        mut run: provenant_fabric_runtime::graph::GraphRun,
        input: &Value,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<provenant_fabric_runtime::graph::GraphRun, FacadeError> {
        let Some(seconds) = input.get("wait_seconds").and_then(Value::as_u64) else {
            return Ok(run);
        };
        let now = self.clock.mono_now();
        let deadline = now
            .checked_add(budget.remaining(now))
            .map_err(|error| FacadeError::invalid(error.to_string()))?;
        let wait = WaitBudget::cli(now, seconds)
            .map_err(|error| FacadeError::invalid(error.to_string()))?
            .cap_at(deadline);
        while run.status == GraphStatus::Pending && !cancel.is_cancelled() {
            let remaining = wait.remaining(self.clock.mono_now());
            if remaining.as_micros() == 0 {
                break;
            }
            provenant_core::Pause::wait(
                &self.backoff,
                provenant_core::DurationMicros::from_micros(250_000.min(remaining.as_micros())),
                cancel,
            );
            run = self.graph(
                GraphInput::Status {
                    extra: Default::default(),
                    id: run.checkpoint.graph_id.clone(),
                },
                budget,
                cancel,
            )?;
        }
        Ok(run)
    }

    fn graph_trace(&self, selector: &str) -> Result<Value, FacadeError> {
        let (selector, node) = selector
            .split_once('/')
            .map_or((selector, None), |(graph, node)| (graph, Some(node)));
        let id = match self.resolve_graph(selector) {
            Ok((id, _)) => id,
            Err(FacadeError::NotFound) => selector.to_owned(),
            Err(error) => return Err(error),
        };
        let mut events = FileJournal::read_events(
            &self.context.roots.state,
            self.context.identity.project,
            &id,
        )
        .map_err(graph_journal_error)?;
        if events.is_empty() {
            return Err(FacadeError::NotFound);
        }
        if let Some(node) = node {
            let subject =
                provenant_records::CanonicalV1::record_digest("fabric-graph-event", 1, &node)
                    .map_err(|_| FacadeError::StateCorrupt)?
                    .to_string();
            events.retain(|event| event.subject == subject);
        }
        Ok(json!({"graph_id":id,"events":events}))
    }

    fn graph_stats(&self, input: &Value) -> Result<Value, FacadeError> {
        let by = input.get("by").and_then(Value::as_str).unwrap_or("tier");
        if !matches!(by, "tier" | "template") {
            return Err(FacadeError::invalid("by must be tier or template"));
        }
        let target = input
            .get("id")
            .and_then(Value::as_str)
            .map(|id| {
                self.graph_target(id)
                    .and_then(|target| target.ok_or(FacadeError::NotFound))
            })
            .transpose()?;
        let ids = match &target {
            Some(target) => vec![target.graph.clone()],
            None => {
                FileJournal::graph_ids(&self.context.roots.state, self.context.identity.project)
                    .map_err(graph_journal_error)?
            }
        };
        let mut groups = BTreeMap::<String, Value>::new();
        let mut seen = BTreeSet::new();
        for id in ids {
            let Some(checkpoint) = FileJournal::read_status_checkpoint(
                &self.context.roots.state,
                self.context.identity.project,
                &id,
            )
            .map_err(graph_journal_error)?
            else {
                continue;
            };
            let execution = FileJournal::execution(
                &self.context.roots.state,
                self.context.identity.project,
                &id,
            )
            .map_err(graph_journal_error)?;
            let document = execution
                .map(|execution| {
                    execution
                        .document
                        .expanded(&self.context.config.graph.limits)
                })
                .transpose()
                .map_err(FacadeError::invalid)?;
            for node in &checkpoint.nodes {
                if target
                    .as_ref()
                    .and_then(|target| target.node.as_ref())
                    .is_some_and(|id| id != &node.id)
                {
                    continue;
                }
                let template = checkpoint
                    .expansions
                    .iter()
                    .rev()
                    .flat_map(|expansion| expansion.nodes.iter())
                    .find(|candidate| candidate.id == node.id)
                    .or_else(|| {
                        document.as_ref().and_then(|document| {
                            document
                                .nodes
                                .iter()
                                .find(|candidate| candidate.id == node.id)
                        })
                    })
                    .and_then(|node| node.extra.get("template"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                for attempt in node.recorded_attempts() {
                    if target
                        .as_ref()
                        .and_then(|target| target.attempt.as_ref())
                        .is_some_and(|id| attempt.attempt_ref.as_ref() != Some(id))
                    {
                        continue;
                    }
                    // Composite totals are never added: only each actual attempt's usage.
                    let identity = attempt.attempt_ref.clone().unwrap_or_else(|| {
                        format!(
                            "{}/{}/{}/{}",
                            attempt.key.graph_id,
                            attempt.key.node_id,
                            attempt.key.route_index,
                            attempt.settled
                        )
                    });
                    if !seen.insert(identity) {
                        continue;
                    }
                    let group = if by == "template" {
                        template
                    } else {
                        attempt.route.alias.as_deref().unwrap_or("unknown")
                    };
                    let row = groups.entry(group.into()).or_insert_with(||json!({"name":group,"attempts":0,"tokens":0,"cost_micro_usd":0,"wall_time_seconds":0,"unknown_usage":0}));
                    add(row, "attempts", 1);
                    if let Some(usage) = &attempt.usage {
                        for (key, value) in [
                            ("tokens", usage.tokens),
                            ("cost_micro_usd", usage.cost_micro_usd),
                            ("wall_time_seconds", usage.wall_time_seconds),
                        ] {
                            if let Some(value) = value {
                                add(row, key, value);
                            }
                        }
                    } else {
                        add(row, "unknown_usage", 1);
                    }
                }
            }
        }
        Ok(json!({"by":by,"groups":groups.into_values().collect::<Vec<_>>()}))
    }
}

fn bind_read_roots(
    node: &mut provenant_fabric_runtime::graph::GraphNode,
    value: &Value,
) -> Result<(), FacadeError> {
    if let Some(roots) = value.get("read_roots") {
        node.read_roots = serde_json::from_value(roots.clone())
            .map_err(|error| FacadeError::invalid(error.to_string()))?;
    }
    if let Some(plan) = value.get("plan") {
        node.plan = Some(
            serde_json::from_value(plan.clone())
                .map_err(|error| FacadeError::invalid(error.to_string()))?,
        );
    }
    if let Some(composite) = &mut node.composite {
        for (child, value) in composite.nodes.iter_mut().zip(
            value
                .pointer("/composite/nodes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten(),
        ) {
            bind_read_roots(child, value)?;
        }
    }
    if let Some(body) = &mut node.loop_body {
        for (child, value) in body.body.iter_mut().zip(
            value
                .pointer("/loop/body")
                .and_then(Value::as_array)
                .into_iter()
                .flatten(),
        ) {
            bind_read_roots(child, value)?;
        }
    }
    Ok(())
}

fn override_tier(
    node: &mut provenant_fabric_runtime::graph::GraphNode,
    route: &provenant_fabric_runtime::graph::GraphRoute,
) {
    if let Some(composite) = &mut node.composite {
        for child in &mut composite.nodes {
            override_tier(child, route);
        }
    } else if let Some(body) = &mut node.loop_body {
        for child in &mut body.body {
            override_tier(child, route);
        }
    } else if !matches!(
        node.kind,
        Some(
            provenant_fabric_runtime::graph::NodeKind::Gate
                | provenant_fabric_runtime::graph::NodeKind::Decide
                | provenant_fabric_runtime::graph::NodeKind::Land
        )
    ) {
        node.route = Some(route.clone());
        node.routes = None;
    }
    if let Some(fallback) = node.plan.as_mut().and_then(|plan| plan.fallback.as_mut()) {
        for child in &mut fallback.nodes {
            override_tier(child, route);
        }
    }
}

fn add(row: &mut Value, key: &str, value: u64) {
    row[key] = json!(row[key].as_u64().unwrap_or(0).saturating_add(value));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worktree_tests::support::{Result, TestRig};

    struct Workers;
    impl WorkerObserver for Workers {
        fn observe_roots(
            &self,
            _: &[PathBuf],
        ) -> BTreeMap<PathBuf, provenant_confinement::worktrees::WorkerState> {
            BTreeMap::new()
        }
    }

    fn factory(rig: &TestRig) -> Result<Factory> {
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        Ok(Factory {
            context: Context::open(roots, &rig.home, &rig.project)?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: rig.home.clone(),
            cwd: rig.project.clone(),
            assets: rig.project.clone(),
            host_launch: None,
            mcp_project: None,
            workers: &Workers,
        })
    }

    #[test]
    fn graph_run_explanation_compiles_brief_tier_over_and_parameters() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
        let response = factory.graph_document(json!({"op":"run","brief":"Inspect ${item}","tier":"junior","over":["one","two"],"explain":true}),budget,&cancel)?;
        let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
        let expanded = document.expanded(&factory.context.config.graph.limits)?;
        let leaves: Vec<_> = expanded
            .nodes
            .iter()
            .filter(|node| node.composite.is_none())
            .collect();
        assert_eq!(leaves.len(), 2);
        assert!(leaves.iter().all(|node| {
            node.route.as_ref().and_then(|route| route.alias.as_deref()) == Some("junior")
        }));
        let prompts: Vec<_> = leaves
            .iter()
            .map(|node| Some(node.prompt.as_str()))
            .collect();
        assert_eq!(prompts, vec![Some("Inspect one"), Some("Inspect two")]);
        let response = factory.graph_document(
            json!({"op":"run","use":"task-once","brief":"Parameter brief","tier":"junior","set":{},"explain":true}),
            budget,
            &cancel,
        )?;
        let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
        assert!(
            document
                .expanded(&factory.context.config.graph.limits)?
                .nodes
                .iter()
                .any(|node| Some(node.prompt.as_str()) == Some("Parameter brief"))
        );
        assert!(
            factory
                .graph_document(json!({"op":"run","document":"graph.json"}), budget, &cancel)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn graph_edit_is_revisioned_replayable_and_journalled() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
        let document: GraphDocument = serde_json::from_value(
            factory.graph_document(
                json!({"op":"run","brief":"Initial","explain":true}),
                budget,
                &cancel,
            )?["document"]
                .clone(),
        )?;
        let id = document.graph_id.clone();
        let mut checkpoint = serde_json::from_value(
            json!({"schema_version":1,"graph_id":id,"plan_digest":"test","nodes":[{"id":"task","worktree":null,"state":"pending","attempts":[]}]}),
        )?;
        let mut journal = FileJournal::open(
            &factory.context.roots.state,
            factory.context.identity.project,
            &id,
            budget,
            &cancel,
            &factory.clock,
        )?;
        journal.bind(provenant_fabric_runtime::graph::GraphExecution {
            extra: Default::default(),
            directory: factory.cwd.clone(),
            document,
            review_base: None,
            integration_base: None,
        })?;
        journal.save(&checkpoint)?;
        drop(journal);
        let edit = json!({"op":"edit","id":id,"patch":{"schema_version":1,"expected_revision":0,"idempotency_key":"add-one","nodes":[{"id":"second","use":"task","brief":"Next"}]}});
        let admitted = factory.graph_document(edit.clone(), budget, &cancel)?;
        assert_eq!(admitted["revision"], 1);
        let replayed = factory.graph_document(edit, budget, &cancel)?;
        assert_eq!(replayed["replayed"], true);
        assert!(factory.graph_document(json!({"op":"edit","id":id,"patch":{"schema_version":1,"expected_revision":0,"nodes":[{"id":"stale","use":"task"}]}}),budget,&cancel).is_err());
        checkpoint = FileJournal::read_status_checkpoint(
            &factory.context.roots.state,
            factory.context.identity.project,
            &id,
        )?
        .ok_or("checkpoint missing")?;
        assert_eq!(
            checkpoint
                .nodes
                .iter()
                .filter(|node| node.id == "second")
                .count(),
            1
        );
        let events =
            factory.graph_document(json!({"op":"output","id":id,"trace":true}), budget, &cancel)?;
        assert!(
            events["events"]
                .as_array()
                .ok_or("events missing")?
                .iter()
                .any(|event| event["kind"] == "patch")
        );
        Ok(())
    }
    #[test]
    fn graph_stats_counts_each_attempt_once_without_composite_totals() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
        let document: GraphDocument = serde_json::from_value(
            factory.graph_document(
                json!({"op":"run","brief":"Initial","explain":true}),
                budget,
                &cancel,
            )?["document"]
                .clone(),
        )?;
        let id = document.graph_id.clone();
        let attempt = json!({"key":{"graph_id":id,"node_id":"task","route_index":0},"launched":true,"route":{"alias":"senior"},"state":"terminal","attempt_ref":"host/attempt","launch_actor":"actor","usage":{"tokens":100,"cost_micro_usd":25,"wall_time_seconds":3}});
        let checkpoint = serde_json::from_value(
            json!({"schema_version":1,"graph_id":id,"plan_digest":"test","nodes":[
                {"id":"group","worktree":null,"state":"succeeded","attempts":[],"children":["task"],"usage":{"tokens":100}},
                {"id":"task","worktree":null,"state":"succeeded","attempts":[attempt.clone()],"previous_attempts":[attempt]}
            ]}),
        )?;
        let mut journal = FileJournal::open(
            &factory.context.roots.state,
            factory.context.identity.project,
            &id,
            budget,
            &cancel,
            &factory.clock,
        )?;
        journal.bind(provenant_fabric_runtime::graph::GraphExecution {
            extra: Default::default(),
            directory: factory.cwd.clone(),
            document,
            review_base: None,
            integration_base: None,
        })?;
        journal.save(&checkpoint)?;
        drop(journal);
        let stats = factory.graph_document(
            json!({"op":"status","id":id,"stats":true,"by":"tier"}),
            budget,
            &cancel,
        )?;
        assert_eq!(stats["groups"][0]["attempts"], 1);
        assert_eq!(stats["groups"][0]["tokens"], 100);
        assert_eq!(stats["groups"][0]["cost_micro_usd"], 25);
        assert_eq!(stats["groups"][0]["wall_time_seconds"], 3);
        let before = FileJournal::read_status_checkpoint(
            &factory.context.roots.state,
            factory.context.identity.project,
            &id,
        )?;
        for extend in [json!(true), json!(["budget.tokens=1000"])] {
            assert!(
                factory
                    .graph_document(
                        json!({"op":"retry","id":format!("{id}/task"),"extend":extend}),
                        budget,
                        &cancel
                    )
                    .is_err()
            );
            assert_eq!(
                FileJournal::read_status_checkpoint(
                    &factory.context.roots.state,
                    factory.context.identity.project,
                    &id
                )?,
                before
            );
        }
        Ok(())
    }
}
