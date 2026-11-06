//! Document-based graph service shared by the CLI and MCP transports.
use super::*;
use provenant_fabric_runtime::graph::{
    FileJournal, GraphDocument, GraphJournal, GraphStatus, Layer, Template, TemplateLibrary,
    patch::{self, LiveGraph, Patch, PatchContext},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn encode(value: impl serde::Serialize) -> Result<Value, FacadeError> {
    serde_json::to_value(value).map_err(|_| FacadeError::StateCorrupt)
}

fn required<'a>(input: &'a Value, field: &str) -> Result<&'a str, FacadeError> {
    input[field]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| FacadeError::invalid(InvalidInputDetail::Field(field.into())))
}

/// Apply plain-task execution controls before template lowering and patch admission.
fn bind_launch_controls(input: &Value, node: &mut Value, cwd: &Path) -> Result<(), FacadeError> {
    let invalid = |field: &str| FacadeError::invalid(InvalidInputDetail::Field(field.into()));
    let branch = input.get("branch").filter(|value| !value.is_null());
    let base = input.get("base").filter(|value| !value.is_null());
    let worktree = input.get("worktree").filter(|value| !value.is_null());
    let access = input.get("access").filter(|value| !value.is_null());
    if branch.is_some() != base.is_some() || branch.is_some() && worktree.is_some() {
        return Err(invalid("branch"));
    }
    if let Some(access) = access {
        if !matches!(access.as_str(), Some("read_only" | "worktree_write")) {
            return Err(invalid("access"));
        }
        node["access"] = access.clone();
    }
    if branch.is_some() || worktree.is_some() {
        if access == Some(&json!("read_only")) {
            return Err(invalid("access"));
        }
        node["access"] = json!("worktree_write");
    }
    if let Some(branch) = branch {
        let branch = branch.as_str().filter(|value| !value.is_empty()).ok_or_else(|| invalid("branch"))?;
        provenant_confinement::repo::GitRefName::branch(branch).map_err(|_| invalid("branch"))?;
        let base = base.and_then(Value::as_str).filter(|value| !value.is_empty()).ok_or_else(|| invalid("base"))?;
        provenant_confinement::repo::GitRevision::new(base).map_err(|_| invalid("base"))?;
        node["worktree"] = json!(format!(".worktrees/{}", branch.replace('/', "-")));
        if node.get("attempt").is_none() {
            node["attempt"] = json!({});
        }
        if !node["attempt"].is_object() {
            return Err(invalid("attempt"));
        }
        node["attempt"]["new_worktree"] = json!({"branch":branch,"base":base});
    } else if let Some(worktree) = worktree {
        let path = Path::new(worktree.as_str().filter(|value| !value.is_empty()).ok_or_else(|| invalid("worktree"))?);
        node["worktree"] = json!(if path.is_absolute() { path.to_owned() } else { cwd.join(path) });
    } else if access == Some(&json!("worktree_write")) && node.get("worktree").is_none() {
        node["worktree"] = json!(cwd);
    }
    if let Some(scope) = input.get("write_scope").filter(|value| !value.is_null()) {
        if !scope.as_array().is_some_and(|paths| paths.iter().all(Value::is_string)) {
            return Err(invalid("write_scope"));
        }
        node["write_scope"] = scope.clone();
    }
    Ok(())
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
        .map_err(|_| FacadeError::InvalidInput { detail: None })?;
        // Resolver selection needs review context, before template nodes compile.
        let mut resolver_document =
            json!({"schema_version":2,"graph_id":"launch-resolvers","nodes":[]});
        for field in ["review", "context"] {
            if let Some(value) = document.and_then(|document| document.get(field)) {
                resolver_document[field] = value.clone();
            }
        }
        let document: GraphDocument = serde_json::from_value(resolver_document)
            .map_err(|_| FacadeError::InvalidInput { detail: None })?;
        provenant_cli::fabric::graph_relay::launch_resolvers(
            &config.domains.routing.review,
            &document,
        )
        .map_err(|_| FacadeError::InvalidInput { detail: None })
    }

    fn relay_document(
        &self,
        input: &Value,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Value, FacadeError> {
        let request = provenant_fabric_mcp::tools::lane_run_request(input)
            .map_err(|_| FacadeError::InvalidInput { detail: None })?;
        if input["explain"] == true {
            return encode(request);
        }
        let within =
            std::time::Duration::from_micros(budget.remaining(self.clock.mono_now()).as_micros());
        let receipt = provenant_cli::fabric::graph_relay::from_lane(
            &request,
            cancel,
            within,
        )
        .map_err(|_| FacadeError::InvalidInput { detail: None })?;
        if let Some(seconds) = input.get("wait_seconds").and_then(Value::as_u64) {
            return Ok(match provenant_cli::fabric::graph_relay::wait_for_children(
                cancel,
                std::time::Duration::from_secs(seconds),
            ) {
                Ok(children) => json!({"receipt":receipt,"children":children}),
                Err(error) => json!({"receipt":receipt,"observation_error":error.to_string()}),
            });
        }
        Ok(receipt)
    }

    fn confine_node(&self, node: &mut Value) -> Result<(), FacadeError> {
        let policy = provenant_fabric_runtime::read_roots::ReadRoots::new(
            crate::graph_support::project_worktrees(&self.cwd),
            &self.context.config.graph.allowed_read_roots,
        );
        crate::graph_support::confine_pattern_roots(node, &policy)
            .map_err(|_| FacadeError::InvalidInput { detail: None })?;
        if let Some(roots) = node.get_mut("read_roots").and_then(Value::as_array_mut) {
            for root in roots {
                let given = root.as_str().ok_or_else(|| {
                    FacadeError::invalid(InvalidInputDetail::Field("read_roots".into()))
                })?;
                let path = Path::new(given);
                let path = if path.is_absolute() {
                    path.to_owned()
                } else {
                    self.cwd.join(path)
                };
                *root = encode(
                    policy
                        .confine(&path)
                        .map_err(|_| FacadeError::InvalidInput { detail: None })?,
                )?;
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
            return Err(FacadeError::invalid(InvalidInputDetail::Field(
                "document".into(),
            )));
        }
        let mut library = TemplateLibrary::bundled();
        library
            .load_dir(Layer::User, &config.join("provenant/templates"))
            .map_err(|_| FacadeError::InvalidInput { detail: None })?;
        library
            .load_dir(Layer::Repository, &self.cwd.join(".provenant/templates"))
            .map_err(|_| FacadeError::InvalidInput { detail: None })?;
        if let Some(inline) = inline.filter(|value| !value.is_null()) {
            let templates: BTreeMap<String, Template> = serde_json::from_value(inline.clone())
                .map_err(|_| FacadeError::InvalidInput { detail: None })?;
            library = library
                .with(Layer::Inline, &templates)
                .map_err(|_| FacadeError::InvalidInput { detail: None })?;
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
            let seconds = wait.as_u64().ok_or_else(|| {
                FacadeError::invalid(InvalidInputDetail::Field("wait_seconds".into()))
            })?;
            WaitBudget::cli(self.clock.mono_now(), seconds)
                .map_err(|_| FacadeError::InvalidInput { detail: None })?;
        }
        if self.context.inspection_only
            && (matches!(operation, "edit" | "stop" | "retry")
                || operation == "run" && input["explain"] != true)
        {
            return Err(FacadeError::ConfinementRefused { resource: None });
        }
        if self.context.in_lane {
            if operation == "run" {
                return self.relay_document(&input, budget, cancel);
            }
            if operation == "edit" {
                let patch: Patch = serde_json::from_value(input["patch"].clone())
                    .map_err(|_| FacadeError::InvalidInput { detail: None })?;
                let op = provenant_fabric_runtime::graph::relay::RelayOp::Edit {
                    id: required(&input, "id")?.to_owned(),
                    patch,
                };
                let within = std::time::Duration::from_micros(
                    budget.remaining(self.clock.mono_now()).as_micros(),
                );
                return provenant_cli::fabric::graph_relay::from_lane(&op, cancel, within)
                    .map_err(|_| FacadeError::InvalidInput { detail: None });
            }
            if matches!(operation, "stop" | "retry") {
                return Err(FacadeError::ConfinementRefused { resource: None });
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
                            "template":library.resolve(name).map_err(|_| FacadeError::InvalidInput { detail: None })?,
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
            _ => Err(FacadeError::invalid(InvalidInputDetail::Field(
                "operation".into(),
            ))),
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
            return Err(FacadeError::invalid(InvalidInputDetail::Field(
                "document".into(),
            )));
        }
        if raw
            .and_then(|document| document.get("integration_branch"))
            .is_some_and(|branch| !branch.is_null() && !branch.is_string())
        {
            return Err(FacadeError::invalid(InvalidInputDetail::Field(
                "integration_branch".into(),
            )));
        }
        let has_overrides = ["brief", "use", "tier", "over", "access", "worktree", "branch", "base", "write_scope"]
            .iter()
            .any(|field| input.get(*field).is_some_and(|value| !value.is_null()))
            || input
                .get("set")
                .is_some_and(|value| value.as_object().is_none_or(|object| !object.is_empty()));
        let lane_document =
            raw.filter(|document| document.get("graph_id").is_none() || has_overrides);
        let mut inline = raw
            .and_then(|document| document.get("templates"))
            .cloned()
            .unwrap_or_else(|| json!({}));
        if let Some(document) = lane_document.filter(|document| document.get("use").is_none()) {
            inline["lane-document"] = document.clone();
        }
        let library = self.template_library(Some(&inline))?;
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
        let mut document = if let Some(raw) = raw.filter(|_| lane_document.is_none()) {
            if raw["nodes"]
                .as_array()
                .is_some_and(|nodes| nodes.iter().any(|node| node.get("use").is_some()))
                || raw.get("templates").is_some()
            {
                let patch = Patch {
                    schema_version: patch::PATCH_SCHEMA_VERSION,
                    nodes: serde_json::from_value(raw["nodes"].clone())
                        .map_err(|_| FacadeError::InvalidInput { detail: None })?,
                    ..Default::default()
                };
                patch::submit(id, &patch, &context, raw["integration_branch"].as_str())
                    .map_err(|_| FacadeError::InvalidInput { detail: None })?
            } else {
                serde_json::from_value::<GraphDocument>(raw.clone())
                    .map_err(|_| FacadeError::InvalidInput { detail: None })?
            }
        } else {
            let mut node = match lane_document {
                Some(document) if document.get("use").is_some() => document.clone(),
                Some(_) => json!({"id":"task", "use":"lane-document"}),
                None => json!({"id":"task", "use":"task"}),
            };
            if node.get("id").is_none() {
                node["id"] = json!("task");
            }
            if let Some(name) = input.get("use").filter(|value| !value.is_null()) {
                node["use"] = name.clone();
            }
            for field in ["brief", "tier", "over"] {
                if let Some(value) = input.get(field).filter(|value| !value.is_null()) {
                    node[field] = value.clone();
                }
            }
            let template = library
                .resolve(node["use"].as_str().unwrap_or("task"))
                .map_err(|_| FacadeError::InvalidInput { detail: None })?;
            if template.is_composite() && ["access", "worktree", "branch", "base", "write_scope"].iter().any(|field| input.get(*field).is_some_and(|value| !value.is_null())) {
                return Err(FacadeError::invalid(InvalidInputDetail::Field("launch controls require a leaf template".into())));
            }
            bind_launch_controls(input, &mut node, &self.cwd)?;
            let mut parameters = node.get("with").cloned().unwrap_or_else(|| json!({}));
            let overrides = input
                .get("set")
                .filter(|value| !value.is_null())
                .cloned()
                .unwrap_or_else(|| json!({}));
            parameters
                .as_object_mut()
                .ok_or_else(|| FacadeError::invalid(InvalidInputDetail::Field("set".into())))?
                .extend(
                    overrides
                        .as_object()
                        .ok_or_else(|| {
                            FacadeError::invalid(InvalidInputDetail::Field("set".into()))
                        })?
                        .clone(),
                );
            if template.parameters.contains_key("brief")
                && let Some(brief) = input.get("brief").filter(|value| !value.is_null())
            {
                parameters["brief"] = brief.clone();
            }
            if template.is_composite() {
                let fields = node.as_object_mut().ok_or_else(|| {
                    FacadeError::invalid(InvalidInputDetail::Field("document".into()))
                })?;
                fields.remove("brief");
                fields.remove("tier");
            }
            // Node parameters supplied explicitly (including CLI --set) beat
            // document settings; template defaults are the final fallback.
            let bundled_batch =
                node["use"] == "batch" && library.layer("batch") == Some(Layer::Bundled);
            let integration_branch = bundled_batch
                .then(|| parameters.get("integration_branch").cloned())
                .flatten()
                .or_else(|| {
                    raw.and_then(|document| document.get("integration_branch"))
                        .filter(|branch| !branch.is_null())
                        .cloned()
                })
                .or_else(|| {
                    bundled_batch
                        .then(|| {
                            template
                                .parameters
                                .get("integration_branch")?
                                .default
                                .clone()
                        })
                        .flatten()
                });
            if bundled_batch && let Some(branch) = &integration_branch {
                parameters["integration_branch"] = branch.clone();
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
                integration_branch.as_ref().and_then(Value::as_str),
            )
            .map_err(|_| FacadeError::InvalidInput { detail: None })?
        };
        if let Some(raw) = raw {
            let mut compiled = encode(&document)?;
            for key in [
                "limits",
                "delivery",
                "review_base",
                "context",
                "decision_session",
                "review",
                "retry",
                "exclude",
                "network",
                "capability",
                "no_capability",
            ] {
                if let Some(value) = raw.get(key) {
                    compiled[key] = value.clone();
                }
            }
            document = serde_json::from_value(compiled)
                .map_err(|_| FacadeError::InvalidInput { detail: None })?;
        }
        // Template compilation selected a schema before document metadata was
        // applied; a retained review base requires the schema-2 representation.
        if raw.is_none()
            || lane_document.is_some()
            || raw.is_some_and(|document| {
                document.get("templates").is_some()
                    || document["nodes"]
                        .as_array()
                        .is_some_and(|nodes| nodes.iter().any(|node| node.get("use").is_some()))
            })
        {
            document.schema_version = document.schema_version.max(u64::from(
                provenant_fabric_runtime::graph::minimum_schema_version(&document),
            ));
        }
        if (raw.is_none() || lane_document.is_some())
            && let Some(tier) = input.get("tier").filter(|value| !value.is_null())
        {
            let tier = tier
                .as_str()
                .ok_or_else(|| FacadeError::invalid(InvalidInputDetail::Field("tier".into())))?;
            let route = serde_json::from_value(json!({"alias":tier}))
                .map_err(|_| FacadeError::StateCorrupt)?;
            for node in &mut document.nodes {
                override_tier(node, &route);
            }
        }
        let mut bound = encode(&document)?;
        self.confine_nodes(&mut bound)?;
        document = serde_json::from_value(bound)
            .map_err(|_| FacadeError::InvalidInput { detail: None })?;
        document
            .admit(&self.context.config.graph.limits)
            .map_err(FacadeError::graph_admission)?;
        let submission_key = format!("submit:{}", document.graph_id);
        patch::submission(
            &mut document,
            &submission_key,
            &self.context.config.graph.limits,
        )
        .map_err(|_| FacadeError::InvalidInput { detail: None })?;
        // Patch admission scans nodes and parameters. Graph-wide metadata is
        // retained separately, so scan the complete bound record before any
        // preview or publication can expose or persist it.
        let bound_document =
            serde_json::to_string(&document).map_err(|_| FacadeError::StateCorrupt)?;
        patch::scan_for_secrets(&bound_document)
            .map_err(|_| FacadeError::InvalidInput { detail: None })?;
        if input["explain"] == true {
            return Ok(json!({"document":document}));
        }
        let scratch = self
            .prepare_scratch(cancel, budget)
            .map_err(scratch_prepare_error)?;
        let path = publish_graph_document(&scratch, &encode(&document)?)
            .map_err(|_| FacadeError::InvalidInput { detail: None })?;
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
        let selector = required(input, "id")?;
        let id = match self.graph_target(selector)? {
            Some(target) => target.graph,
            None => self.resolve_graph(selector)?.0,
        };
        let patch: Patch = serde_json::from_value(input["patch"].clone())
            .map_err(|_| FacadeError::InvalidInput { detail: None })?;
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
        .map_err(|_| FacadeError::InvalidInput { detail: None })?;
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
                return Err(FacadeError::invalid(InvalidInputDetail::Field(
                    "over".into(),
                )));
            }
            target.node = Some(node.into());
        }
        let extend: Vec<String> = input
            .get("extend")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| FacadeError::InvalidInput { detail: None })?
            .unwrap_or_default();
        if !extend.is_empty()
            && (target.node.is_some() || input.get("brief").is_some_and(|value| !value.is_null()))
        {
            return Err(FacadeError::invalid(InvalidInputDetail::Field(
                "node".into(),
            )));
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
            .map_err(|_| FacadeError::InvalidInput { detail: None })?;
            journal.save(&checkpoint).map_err(graph_journal_error)?;
        } else if input.get("brief").is_some_and(|value| !value.is_null()) {
            return Err(FacadeError::invalid(InvalidInputDetail::Field(
                "brief".into(),
            )));
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
                .map_err(|_| FacadeError::InvalidInput { detail: None })?
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
        let mut rows = super::unified::status_rows(&run, target);
        super::unified::enrich_status_rows(&mut rows, &self.context, &self.clock, cancel)?;
        Ok(json!({"rows":rows,"timed_out":timed_out}))
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
            .map_err(|_| FacadeError::InvalidInput { detail: None })?;
        let wait = WaitBudget::cli(now, seconds)
            .map_err(|_| FacadeError::InvalidInput { detail: None })?
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
            return Err(FacadeError::invalid(InvalidInputDetail::Field(
                "tier".into(),
            )));
        }
        let ids =
            FileJournal::event_graph_ids(&self.context.roots.state, self.context.identity.project)
                .map_err(graph_journal_error)?;
        let selector = input.get("id").and_then(Value::as_str);
        let target = if let Some(id) = selector {
            if ids.iter().any(|graph| graph == id) {
                Some(super::unified::GraphTarget {
                    graph: id.into(),
                    node: None,
                    attempt: None,
                })
            } else {
                Some(self.graph_target(id)?.ok_or(FacadeError::NotFound)?)
            }
        } else {
            None
        };
        let mut attempts = BTreeMap::new();
        for id in ids {
            if target.as_ref().is_some_and(|target| target.graph != id) {
                continue;
            }
            let subject = target
                .as_ref()
                .and_then(|target| target.node.as_ref())
                .map(|node| {
                    provenant_records::CanonicalV1::record_digest("fabric-graph-event", 1, node)
                        .map(|digest| digest.to_string())
                        .map_err(|_| FacadeError::StateCorrupt)
                })
                .transpose()?;
            let selected_attempt = target
                .as_ref()
                .and_then(|target| target.attempt.as_ref())
                .map(|attempt| {
                    provenant_records::CanonicalV1::record_digest("fabric-graph-event", 1, attempt)
                        .map(|digest| digest.to_string())
                        .map_err(|_| FacadeError::StateCorrupt)
                })
                .transpose()?;
            for event in FileJournal::read_events(
                &self.context.roots.state,
                self.context.identity.project,
                &id,
            )
            .map_err(graph_journal_error)?
            {
                if subject
                    .as_ref()
                    .is_some_and(|subject| *subject != event.subject)
                {
                    continue;
                }
                if let Some(attempt) = event.attempt {
                    if selected_attempt
                        .as_ref()
                        .is_some_and(|selected| *selected != attempt.identity)
                    {
                        continue;
                    }
                    attempts.insert(attempt.identity.clone(), attempt);
                }
            }
        }
        let mut groups = BTreeMap::<String, Value>::new();
        for attempt in attempts.into_values() {
            let group = if by == "template" {
                &attempt.template
            } else {
                &attempt.tier
            };
            let row = groups.entry(group.clone()).or_insert_with(|| json!({"name":group,"attempts":0,"tokens":0,"cost_micro_usd":0,"wall_time_seconds":0,"unknown_usage":0}));
            add(row, "attempts", 1);
            for (key, value) in [
                ("tokens", attempt.tokens),
                ("cost_micro_usd", attempt.cost_micro_usd),
                ("wall_time_seconds", attempt.wall_time_seconds),
            ] {
                if let Some(value) = value {
                    add(row, key, value);
                }
            }
            if !attempt.has_usage {
                add(row, "unknown_usage", 1);
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
            .map_err(|_| FacadeError::InvalidInput { detail: None })?;
    }
    if let Some(plan) = value.get("plan") {
        node.plan = Some(
            serde_json::from_value(plan.clone())
                .map_err(|_| FacadeError::InvalidInput { detail: None })?,
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
#[allow(
    dead_code,
    unused_imports,
    reason = "the custom worktree driver omits libtest functions"
)]
mod tests {
    use super::*;
    use crate::worktree_tests::support::{Result, TestRig};

    struct Workers;
    impl WorkerObserver for Workers {
        fn observe_roots(
            &self,
            _: &[PathBuf],
        ) -> BTreeMap<PathBuf, provenant_confinement::worktrees::Occupancy> {
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
    fn plain_task_launch_controls_bind_writer_and_route_before_admission() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
        let response = factory.graph_document(json!({
            "op":"run", "brief":"Fix the parser", "tier":"senior",
            "worktree":rig.project, "write_scope":["src"], "explain":true
        }), budget, &cancel)?;
        let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
        assert_eq!(document.nodes.len(), 1);
        let node = &document.nodes[0];
        assert_eq!(node.access, provenant_fabric_runtime::graph::AccessMode::WorktreeWrite);
        assert_eq!(node.worktree.as_deref(), rig.project.to_str());
        assert_eq!(node.write_scope, vec!["src"]);
        assert_eq!(node.route.as_ref().and_then(|route| route.alias.as_deref()), Some("senior"));
        Ok(())
    }

    #[test]
    fn writer_branch_explanation_is_one_admitted_node_without_git_effects() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
        let cancel = CancelToken::new();
        let response = factory.graph_document(json!({
            "op":"run", "brief":"Fix the parser", "branch":"fix/parser",
            "base":"HEAD", "explain":true
        }), budget, &cancel)?;
        let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
        assert_eq!(document.nodes.len(), 1);
        let node = &document.nodes[0];
        assert_eq!(node.worktree.as_deref(), Some(".worktrees/fix-parser"));
        let target = node.attempt.as_ref().and_then(|spec| spec.new_worktree.as_ref()).ok_or("new worktree missing")?;
        assert_eq!(target.branch, "fix/parser");
        assert_eq!(target.base, "HEAD");
        assert!(!rig.project.join(".worktrees/fix-parser").exists());
        assert!(factory.graph_document(json!({"op":"run","use":"deliver","worktree":rig.project,"explain":true}), budget, &cancel).is_err());
        Ok(())
    }

    #[test]
    fn writer_controls_preserve_new_branch_and_refuse_conflicting_access() -> Result {
        let cwd = Path::new("/fixture/project");
        let mut node = json!({"id":"task","use":"task"});
        bind_launch_controls(&json!({"branch":"fix/parser","base":"origin/main"}), &mut node, cwd)?;
        assert_eq!(node["worktree"], ".worktrees/fix-parser");
        assert_eq!(node["attempt"]["new_worktree"], json!({"branch":"fix/parser","base":"origin/main"}));
        assert_eq!(node["access"], "worktree_write");
        for input in [
            json!({"worktree":".","access":"read_only"}),
            json!({"branch":"fix/parser"}),
            json!({"base":"HEAD"}),
            json!({"branch":"fix/parser","base":"HEAD","worktree":"."}),
            json!({"access":"unknown"}),
            json!({"write_scope":[1]}),
        ] {
            assert!(bind_launch_controls(&input, &mut json!({"use":"task"}), cwd).is_err());
        }
        Ok(())
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
        let response = factory.graph_document(json!({"op":"run","document":{"use":"task","brief":"Inspect ${item}"},"over":["one","two"],"explain":true}), budget, &cancel)?;
        let doc: GraphDocument = serde_json::from_value(response["document"].clone())?;
        let prompts: Vec<_> = doc
            .expanded(&factory.context.config.graph.limits)?
            .nodes
            .into_iter()
            .filter(|node| node.composite.is_none())
            .map(|node| node.prompt)
            .collect();
        assert_eq!(prompts, vec!["Inspect one", "Inspect two"]);
        let response = factory.graph_document(json!({"op":"run","document":{"parameters":{"subject":{"type":"string"}},"nodes":[{"id":"one","use":"task","brief":"Inspect ${subject} ${item}"}]},"set":{"subject":"code"},"over":["a","b"],"explain":true}), budget, &cancel)?;
        let doc: GraphDocument = serde_json::from_value(response["document"].clone())?;
        let prompts: Vec<_> = doc
            .expanded(&factory.context.config.graph.limits)?
            .nodes
            .into_iter()
            .filter(|node| node.composite.is_none())
            .map(|node| node.prompt)
            .collect();
        assert_eq!(prompts, vec!["Inspect code a", "Inspect code b"]);
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
    fn graph_context_secret_is_rejected_before_preview_publication_or_journal() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
        let secret = "sk-abcdefghijklmnopqrstuvwx";
        // A compiled document bypasses template scanning. The credential is
        // only in graph context, outside submission's nodes and parameters.
        let document = json!({
            "schema_version":2, "graph_id":"metadata-secret",
            "context":{"retained_note":secret},
            "nodes":[{"id":"inspect", "prompt":"Inspect code", "access":"read_only", "route":{"alias":"junior"}}]
        });
        for explain in [true, false] {
            let error = factory
                .graph_document(
                    json!({
                        "op":"run", "document":document, "explain":explain
                    }),
                    budget,
                    &cancel,
                )
                .err()
                .ok_or("context credential was accepted")?;
            assert_eq!(error, FacadeError::InvalidInput { detail: None });
            assert!(!serde_json::to_string(&error)?.contains(secret));
            assert!(!serde_json::to_string(&error.diagnostic())?.contains(secret));
            assert!(!rig.project.join(".provenant/scratch/graphs").exists());
            assert!(
                FileJournal::graph_ids(
                    &factory.context.roots.state,
                    factory.context.identity.project
                )?
                .is_empty()
            );
            assert!(
                FileJournal::event_graph_ids(
                    &factory.context.roots.state,
                    factory.context.identity.project
                )?
                .is_empty()
            );
        }
        Ok(())
    }

    #[test]
    fn generated_graph_id_preserves_document_binding_and_settings() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
        let settings = json!({
            "integration_branch":"batch/document",
            "limits":{"nodes":128},
            "delivery":"none",
            "review_base":"1111111111111111111111111111111111111111",
            "context":{"risk_tier":"substantial", "task_kind":"implementation"},
            "decision_session":"chair-session",
            "review":{"risk_tier":"substantial"},
            "retry":{"retries":1},
            "exclude":["codex/gpt-6-luna"],
            "network":"off",
            "capability":["postgres"],
            "no_capability":[]
        });
        let mut source = settings.clone();
        source["nodes"] = json!([{
            "id":"inspect", "use":"task", "source":{"type":"integration"}
        }]);
        let response = factory.graph_document(
            json!({"op":"run", "document":source, "explain":true}),
            budget,
            &cancel,
        )?;
        let compiled = &response["document"];
        assert!(
            compiled["graph_id"]
                .as_str()
                .is_some_and(|id| id.starts_with("g-"))
        );
        for (field, expected) in settings.as_object().ok_or("settings object")? {
            assert_eq!(&compiled[field], expected, "lost graph setting {field}");
        }
        for invalid_branch in [
            json!(7),
            json!({"branch":"batch/document"}),
            json!(["batch/document"]),
        ] {
            let error = factory.graph_document(json!({
                "op":"run", "explain":true,
                "document":{"integration_branch":invalid_branch,"nodes":[{"id":"task","use":"task"}]}
            }), budget, &cancel).err().ok_or("invalid branch was accepted")?;
            assert_eq!(
                error,
                FacadeError::invalid(InvalidInputDetail::Field("integration_branch".into()))
            );
        }
        let document: GraphDocument = serde_json::from_value(compiled.clone())?;
        assert!(
            document
                .expanded(&factory.context.config.graph.limits)?
                .nodes
                .iter()
                .any(|node| {
                    matches!(
                        node.source,
                        Some(provenant_fabric_runtime::graph::SourceBinding::Integration)
                    )
                })
        );
        Ok(())
    }

    #[test]
    fn explicit_batch_node_and_cli_branch_parameters_override_document_binding() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
        for (node_parameters, parameters, expected) in [
            (json!({}), json!({}), "batch/document"),
            (
                json!({"integration_branch":"batch/node"}),
                json!({}),
                "batch/node",
            ),
            (
                json!({"integration_branch":"batch/node"}),
                json!({"integration_branch":"batch/cli"}),
                "batch/cli",
            ),
        ] {
            let mut bindings = json!({"worktree":rig.project,"write_scope":["."]});
            bindings.as_object_mut().ok_or("bindings object")?.extend(
                node_parameters
                    .as_object()
                    .ok_or("parameters object")?
                    .clone(),
            );
            let response = factory.graph_document(
                json!({
                    "op":"run", "explain":true, "set":parameters,
                    "document":{
                        "use":"batch", "integration_branch":"batch/document",
                        "with":bindings
                    }
                }),
                budget,
                &cancel,
            )?;
            assert_eq!(response["document"]["integration_branch"], expected);
            let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
            assert!(
                document
                    .expanded(&factory.context.config.graph.limits)?
                    .nodes
                    .iter()
                    .any(|node| {
                        node.delivery
                            == Some(provenant_records::run::DeliveryMode::Integrate(
                                expected.into(),
                            ))
                    })
            );
        }
        Ok(())
    }

    #[test]
    fn writer_sourced_template_review_selects_shape_and_pins_every_stage() -> Result {
        use provenant_fabric_runtime::graph::{self, AttemptOwner};
        #[derive(Default)]
        struct Journal(Option<graph::GraphCheckpoint>);
        impl graph::GraphJournal for Journal {
            type Error = std::convert::Infallible;
            fn load(
                &mut self,
                _: &str,
            ) -> std::result::Result<Option<graph::GraphCheckpoint>, Self::Error> {
                Ok(self.0.clone())
            }
            fn save(
                &mut self,
                checkpoint: &graph::GraphCheckpoint,
            ) -> std::result::Result<(), Self::Error> {
                self.0 = Some(checkpoint.clone());
                Ok(())
            }
        }
        struct Owner {
            source: String,
            commit: String,
            reviewed: BTreeMap<String, Option<String>>,
        }
        impl AttemptOwner for Owner {
            type Error = std::convert::Infallible;
            fn actor(&self) -> String {
                "template-review-fixture".into()
            }
            fn bind_writer_worktree(
                &mut self,
                worktree: &str,
            ) -> std::result::Result<Option<graph::WriterWorktreeBinding>, Self::Error>
            {
                Ok(Some(graph::WriterWorktreeBinding {
                    extra: Default::default(),
                    worktree: worktree.into(),
                    head: self.source.clone(),
                }))
            }
            fn observe_or_start(
                &mut self,
                key: &graph::AttemptKey,
                node: graph::NodeInput<'_>,
                _: &graph::GraphRoute,
                access: graph::AccessMode,
                _: Option<&str>,
                _: graph::Replay<'_>,
            ) -> std::result::Result<graph::AttemptObservation, Self::Error> {
                let writer = key.node_id == "writer";
                if !writer {
                    assert_eq!(access, graph::AccessMode::ReadOnly);
                    self.reviewed
                        .insert(key.node_id.clone(), node.source_commit.map(str::to_owned));
                }
                let mut result =
                    provenant_records::result_capture::ResultCapture::new(node.required_verdict);
                result.push(
                    b"```findings\n{\"schema_version\":1,\"findings\":[]}\n```\nVERDICT: PASS\n",
                );
                Ok(graph::AttemptObservation::Terminal {
                    attempt_ref: Some(format!("fixture/{}", key.node_id)),
                    outcome: graph::AttemptOutcome::Succeeded,
                    launched: true,
                    result: Some(Box::new(result.finish())),
                    result_warning: None,
                    result_path: None,
                    branch: writer.then(|| "slice".into()),
                    commit: writer.then(|| self.commit.clone()),
                })
            }
        }
        let rig = crate::worktree_tests::repository()?;
        let original = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
        crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "writer subject"])?;
        let writer_commit = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
        let factory = factory(&rig)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        for (risk, full_panel) in [("routine", false), ("substantial", true)] {
            let input = json!({
                "op":"run", "explain":true,
                "document":{
                    "schema_version":2, "graph_id":format!("writer-review-{risk}"),
                    "review_base":original, "context":{"risk_tier":risk},
                    "nodes":[
                        {"id":"writer", "use":"implement", "worktree":rig.project, "write_scope":["."], "delivery":"push"},
                        {"id":"review", "use":"review-panel", "after":["writer"], "source":{"type":"head","node":"writer"}}
                    ]
                }
            });
            let response = factory
                .graph_document(input, budget, &cancel)
                .map_err(|error| format!("{risk} public template submission: {error:?}"))?;
            let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
            let expanded = document.expanded(&factory.context.config.graph.limits)?;
            let review_stages: std::collections::BTreeSet<String> = expanded
                .nodes
                .iter()
                .filter(|node| node.id.starts_with("review--") && node.composite.is_none())
                .map(|node| node.id.clone())
                .collect();
            for stage in ["frame", "verify", "synthesis"] {
                assert_eq!(
                    review_stages
                        .iter()
                        .any(|id| id.contains(&format!("--{stage}"))),
                    full_panel,
                    "{risk}: {stage}"
                );
            }
            if !full_panel {
                assert_eq!(review_stages.len(), 1);
            }
            // A moving checkout cannot change the immutable commit supplied by
            // the completed writer to the review template and all its stages.
            crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "later head"])?;
            assert_ne!(
                crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?,
                writer_commit
            );
            let mut owner = Owner {
                source: original.clone(),
                commit: writer_commit.clone(),
                reviewed: BTreeMap::new(),
            };
            let run = graph::advance_with_budget(
                &document,
                &mut owner,
                &mut Journal::default(),
                &factory.context.config.graph.limits,
                &factory.context.config.graph.budget,
            )?;
            assert_eq!(run.status, graph::GraphStatus::Complete);
            assert!(run.checkpoint.successful(), "{risk}: {:?}", run.checkpoint);
            assert_eq!(
                owner
                    .reviewed
                    .keys()
                    .cloned()
                    .collect::<std::collections::BTreeSet<_>>(),
                review_stages
            );
            for (id, source) in owner.reviewed {
                assert_eq!(
                    source.as_deref(),
                    Some(writer_commit.as_str()),
                    "{risk}: {id}"
                );
                assert_eq!(
                    run.checkpoint
                        .nodes
                        .iter()
                        .find(|node| node.id == id)
                        .ok_or("review checkpoint")?
                        .source_commit
                        .as_deref(),
                    Some(writer_commit.as_str())
                );
            }
        }
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
            json!({"schema_version":1,"graph_id":id,"plan_digest":"test","nodes":[{"id":"task","worktree":null,"state":"running","attempts":[]}]}),
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
        let edit = json!({"op":"edit","id":format!("{id}/task"),"patch":{"schema_version":1,"expected_revision":0,"idempotency_key":"add-one","nodes":[{"id":"second","use":"task","brief":"Next"}]}});
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
        let checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint = serde_json::from_value(
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
        let mut reserved = checkpoint.clone();
        reserved.nodes[1].attempts[0].attempt_ref = None;
        reserved.nodes[1].previous_attempts.clear();
        journal.save(&reserved)?;
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
        assert_eq!(
            FileJournal::collect_terminal(
                &factory.context.roots.state,
                factory.context.identity.project,
                i128::MAX,
                &cancel,
                &factory.clock
            )?,
            1
        );
        let retained = factory.graph_document(
            json!({"op":"status","id":id,"stats":true,"by":"tier"}),
            budget,
            &cancel,
        )?;
        assert_eq!(retained["groups"][0]["attempts"], 1);
        assert_eq!(retained["groups"][0]["tokens"], 100);
        let all = factory.graph_document(
            json!({"op":"status","stats":true,"by":"template"}),
            budget,
            &cancel,
        )?;
        assert_eq!(all["groups"][0]["attempts"], 1);
        Ok(())
    }
}
