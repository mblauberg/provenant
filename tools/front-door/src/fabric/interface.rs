//! Document-based graph service shared by the CLI and MCP transports.
use super::*;
use provenant_fabric_runtime::coordination::facade::is_map_key_parent;
use provenant_fabric_runtime::graph::{
    FileJournal, GraphDocument, GraphJournal, GraphStatus, Layer, Template, TemplateLibrary,
    patch::{self, LiveGraph, Patch, PatchContext},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn graph_input_reason(reason: impl std::fmt::Display) -> FacadeError {
    FacadeError::invalid(InvalidInputDetail::graph_reason(reason))
}

// Do not expose serde's message: it can contain supplied values. Preserve only
// a schema path and a bounded reason, both filtered at the diagnostic boundary.
fn decode_input<T: serde::de::DeserializeOwned>(
    value: Value,
    prefix: &str,
) -> Result<T, FacadeError> {
    use serde_path_to_error::Segment;
    serde_path_to_error::deserialize(value).map_err(|error| {
        let message = error.inner().to_string();
        let missing = message
            .strip_prefix("missing field `")
            .and_then(|s| s.split_once('`'))
            .map(|(field, _)| field);
        let mut field = prefix.to_owned();
        // Build from segments, stopping at a user map field so no key is kept.
        let mut in_map = false;
        for segment in error.path().iter() {
            match segment {
                Segment::Seq { index } => field.push_str(&format!("[{index}]")),
                Segment::Map { key } => {
                    if !field.is_empty() {
                        field.push('.');
                    }
                    field.push_str(key);
                    if is_map_key_parent(key) {
                        in_map = true;
                        break;
                    }
                }
                Segment::Enum { .. } | Segment::Unknown => {}
            }
        }
        let missing = missing.filter(|_| !in_map);
        if let Some(missing) = missing {
            if !field.is_empty() {
                field.push('.');
            }
            field.push_str(missing);
        }
        FacadeError::invalid(InvalidInputDetail::FieldDecode {
            field,
            missing: missing.is_some(),
        })
    })
}

fn retry_patch_error(error: patch::PatchError) -> FacadeError {
    use provenant_fabric_runtime::coordination::facade::GraphRelayCause;
    let cause = if error.message.starts_with("unknown node ") {
        GraphRelayCause::RetryUnknownNode
    } else if error.message.contains("settled") {
        GraphRelayCause::RetryUnsettled
    } else if error.message.contains("container retry") {
        GraphRelayCause::RetryContainerBrief
    } else if error.admission.is_some() {
        return graph_patch_error(error);
    } else {
        GraphRelayCause::RetryInvalidConfiguration
    };
    FacadeError::invalid(InvalidInputDetail::GraphRelayCause(cause))
}

/// A refused template parameter keeps its typed refusal; any other compile
/// failure stays a redacted graph-input reason.
fn compile_error(error: provenant_fabric_runtime::graph::template::CompileError) -> FacadeError {
    use provenant_fabric_runtime::graph::template::CompileError;
    match error {
        CompileError::Parameter(refusal) => {
            FacadeError::invalid(InvalidInputDetail::Parameter(Box::new(refusal)))
        }
        other => graph_input_reason(other),
    }
}

/// A compile failure that reached graph admission keeps its typed refusal, so
/// a template launch reads the same message as the raw document it compiles to.
/// Failures before admission stay a redacted graph-input reason.
fn graph_patch_error(error: patch::PatchError) -> FacadeError {
    match error.admission.clone() {
        Some(admission) => FacadeError::graph_admission(admission),
        None => match error.parameter.clone() {
            Some(refusal) => FacadeError::invalid(InvalidInputDetail::Parameter(refusal)),
            None => graph_input_reason(error),
        },
    }
}

pub(super) fn relay_error(error: ServiceError) -> FacadeError {
    match error {
        ServiceError::Facade(error) => error,
        ServiceError::Cli { code, .. } if code == "UPGRADE_DRAIN_REQUIRED" => {
            FacadeError::GraphRelayRefused {
                refusal_code: code,
                detail: Some(InvalidInputDetail::UpgradeDrainRequired),
            }
        }
        error => FacadeError::GraphRelayRefused {
            refusal_code: error.code().to_owned(),
            detail: Some(InvalidInputDetail::graph_reason(error)),
        },
    }
}

fn encode(value: impl serde::Serialize) -> Result<Value, FacadeError> {
    serde_json::to_value(value).map_err(|_| FacadeError::StateCorrupt)
}

fn required<'a>(input: &'a Value, field: &str) -> Result<&'a str, FacadeError> {
    input[field]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| FacadeError::invalid(InvalidInputDetail::Field(field.into())))
}

/// Observe a submitted graph within the caller's original wait budget.
pub(super) fn wait_submission(
    mut run: provenant_fabric_runtime::graph::GraphRun,
    wait: WaitBudget,
    clock: &dyn Clock,
    pause: &dyn provenant_core::Pause,
    cancel: &CancelToken,
    mut observe: impl FnMut(&str) -> Result<provenant_fabric_runtime::graph::GraphRun, FacadeError>,
) -> Result<provenant_fabric_runtime::graph::GraphRun, FacadeError> {
    while run.status == GraphStatus::Pending && !cancel.is_cancelled() {
        let remaining = wait.remaining(clock.mono_now());
        if remaining.as_micros() == 0 {
            break;
        }
        pause.wait(
            provenant_core::DurationMicros::from_micros(250_000.min(remaining.as_micros())),
            cancel,
        );
        run = observe(&run.checkpoint.graph_id)?;
    }
    Ok(run)
}

/// Attempt settings are reserved launch controls, not template parameters.
fn launch_input(input: &Value) -> Result<Value, FacadeError> {
    let mut input = input.clone();
    let invalid = || FacadeError::invalid(InvalidInputDetail::Field("attempt".into()));
    let mut attempt = input.get("attempt").cloned().unwrap_or_else(|| json!({}));
    if !attempt.is_object() {
        return Err(invalid());
    }
    if let Some(settings) = input.get_mut("set").and_then(Value::as_object_mut) {
        if let Some(value) = settings.remove("attempt") {
            attempt
                .as_object_mut()
                .ok_or_else(invalid)?
                .extend(value.as_object().ok_or_else(invalid)?.clone());
        }
        let keys: Vec<_> = settings
            .keys()
            .filter(|key| key.starts_with("attempt."))
            .cloned()
            .collect();
        for key in keys {
            let value = settings.remove(&key).ok_or_else(invalid)?;
            attempt[&key[8..]] = value;
        }
    }
    for field in ["host", "account"] {
        if let Some(value) = input.get(field).filter(|value| !value.is_null()) {
            attempt[field] = value.clone();
        }
    }
    let typed: provenant_fabric_runtime::graph::AttemptSpec =
        decode_input(attempt.clone(), "attempt")?;
    for (field, invalid_value) in [
        (
            "attempt.provider_timeout",
            typed.provider_timeout == Some(0),
        ),
        (
            "attempt.host",
            typed.host.as_ref().is_some_and(|host| host.is_empty()),
        ),
    ] {
        if invalid_value {
            return Err(FacadeError::invalid(InvalidInputDetail::FieldDecode {
                field: field.into(),
                missing: false,
            }));
        }
    }
    input["attempt"] = attempt;
    if let Some(value) = input.get("operation_id").filter(|value| !value.is_null()) {
        value
            .as_str()
            .ok_or_else(invalid)?
            .parse::<provenant_core::OperationId>()
            .map_err(|_| invalid())?;
    }
    Ok(input)
}

/// Run-file operation IDs belong to the envelope, not the template schema.
fn run_envelope(input: &Value) -> Result<Value, FacadeError> {
    let mut input = input.clone();
    let nested = input
        .get_mut("document")
        .and_then(Value::as_object_mut)
        .and_then(|fields| fields.remove("operation_id"));
    if input.get("operation_id").is_none()
        && let Some(operation) = nested
    {
        input["operation_id"] = operation;
    }
    if let Some(operation) = input.get("operation_id").filter(|value| !value.is_null()) {
        let id = operation.as_str().ok_or_else(|| {
            FacadeError::invalid(InvalidInputDetail::Field("operation_id".into()))
        })?;
        provenant_core::OperationId::parse(id)
            .map_err(|_| FacadeError::invalid(InvalidInputDetail::Field("operation_id".into())))?;
    }
    Ok(input)
}

fn override_launch(
    node: &mut provenant_fabric_runtime::graph::GraphNode,
    input: &Value,
) -> Result<(), FacadeError> {
    if let Some(composite) = &mut node.composite {
        for child in &mut composite.nodes {
            override_launch(child, input)?;
        }
    } else if let Some(body) = &mut node.loop_body {
        for child in &mut body.body {
            override_launch(child, input)?;
        }
    } else if !matches!(
        node.kind,
        Some(
            provenant_fabric_runtime::graph::NodeKind::Gate
                | provenant_fabric_runtime::graph::NodeKind::Decide
                | provenant_fabric_runtime::graph::NodeKind::Land
        )
    ) {
        let invalid = || FacadeError::invalid(InvalidInputDetail::Field("route".into()));
        if let Some(model) = input.get("model").filter(|value| !value.is_null()) {
            let model = provenant_routing::ModelId::parse(model.as_str().ok_or_else(invalid)?)
                .map_err(|_| invalid())?;
            node.route = Some(
                serde_json::from_value(json!({"adapter":model.adapter(), "model":model.as_str()}))
                    .map_err(|_| invalid())?,
            );
            node.routes = None;
        }
        for route in node
            .route
            .iter_mut()
            .chain(node.routes.iter_mut().flatten())
        {
            if let Some(effort) = input.get("effort").filter(|value| !value.is_null()) {
                route.effort = Some(effort.as_str().ok_or_else(invalid)?.into());
            }
            if let Some(account) = input.get("account").filter(|value| !value.is_null()) {
                route.account = Some(account.as_str().ok_or_else(invalid)?.into());
            }
        }
        if input["attempt"]
            .as_object()
            .is_some_and(|fields| !fields.is_empty())
        {
            let mut attempt = encode(node.attempt.clone().unwrap_or_default())?;
            attempt
                .as_object_mut()
                .ok_or_else(invalid)?
                .extend(input["attempt"].as_object().ok_or_else(invalid)?.clone());
            node.attempt = Some(serde_json::from_value(attempt).map_err(|_| invalid())?);
        }
    }
    if let Some(fallback) = node.plan.as_mut().and_then(|plan| plan.fallback.as_mut()) {
        for child in &mut fallback.nodes {
            override_launch(child, input)?;
        }
    }
    Ok(())
}

fn bind_submission_subject(
    node: &mut provenant_fabric_runtime::graph::GraphNode,
    head: &str,
    subject_missing: bool,
) {
    // An explicit composite source is inherited by its members. Loops resolve
    // their own subjects as writers advance, rather than freezing every round.
    if node.source.is_some() || node.loop_body.is_some() {
        return;
    }
    if let Some(composite) = &mut node.composite {
        for child in &mut composite.nodes {
            bind_submission_subject(child, head, subject_missing);
        }
    } else if node.extra.contains_key("review_behaviour") {
        if subject_missing {
            node.prompt.insert_str(0, "No review subject was supplied on the command line; no host diff range is available. Request a pinned subject and base if the brief requires a diff.\n\n");
        }
        node.prompt.push_str("\n\nWhen a review range is supplied, the host pins its commits and supplies Git context. The review lane has read-only Git metadata and cannot fetch; read the supplied commits and context.");
        node.source = Some(provenant_fabric_runtime::graph::SourceBinding::Commit {
            commit: head.into(),
        });
    }
}

/// Apply plain-task execution controls before template lowering and patch admission.
fn bind_launch_controls(input: &Value, node: &mut Value, cwd: &Path) -> Result<(), FacadeError> {
    let invalid = |field: &str| FacadeError::invalid(InvalidInputDetail::Field(field.into()));
    let branch = input.get("branch").filter(|value| !value.is_null());
    let base = input.get("base").filter(|value| !value.is_null());
    let worktree = input.get("worktree").filter(|value| !value.is_null());
    let access = input.get("access").filter(|value| !value.is_null());
    if base.is_some() && branch.is_none() || branch.is_some() && worktree.is_some() {
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
        let branch = branch
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| invalid("branch"))?;
        provenant_confinement::repo::GitRefName::branch(branch).map_err(|_| invalid("branch"))?;
        let base = match base {
            Some(base) => base
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| invalid("base"))?,
            None => "refs/remotes/origin/HEAD",
        };
        provenant_confinement::repo::GitRevision::new(base).map_err(|_| invalid("base"))?;
        node["worktree"] = json!(format!(".worktrees/{}", branch.replace('/', "-")));
        if node.get("attempt").is_none() {
            node["attempt"] = json!({});
        }
        if !node["attempt"].is_object() {
            return Err(invalid("attempt"));
        }
        node["attempt"]["new_worktree"] = json!({"branch":branch,"base":base});
        if input.get("base").is_none_or(Value::is_null) {
            node["attempt"]["new_worktree"]["default_base"] = json!(true);
        }
    } else if let Some(worktree) = worktree {
        let path = Path::new(
            worktree
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| invalid("worktree"))?,
        );
        node["worktree"] = json!(if path.is_absolute() {
            path.to_owned()
        } else {
            cwd.join(path)
        });
    }
    if let Some(scope) = input.get("write_scope").filter(|value| !value.is_null()) {
        if !scope
            .as_array()
            .is_some_and(|paths| paths.iter().all(Value::is_string))
        {
            return Err(invalid("write_scope"));
        }
        node["write_scope"] = scope.clone();
    }
    Ok(())
}

#[cfg(test)]
mod launch_report_tests {
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn launch_report_distinguishes_refusal_mixed_and_fallback() {
        let rows = vec![
            json!({"kind":"attempt", "attempt_id":"host/one", "settlement_reason":"pre_launch_refusal", "preparation_failure":"auth_required"}),
            json!({"kind":"attempt", "attempt_id":"host/two", "settlement_reason":"pre_launch_refusal", "preparation_failure":"usage_limited"}),
        ];
        let attempt = |id: &str, launched: bool| json!({"attempt_ref":id,"launched":launched});
        let mut all = json!({"status":"complete","outcome":"failed","checkpoint":{"nodes":[{"attempts":[attempt("host/one", false)]},{"attempts":[attempt("host/two", false)]}]}});
        report_launch_refusals(&mut all, &rows);
        assert_eq!(all["outcome"], "failed");
        assert_eq!(
            all["launch_refusals"][0]["preparation_failure"],
            "auth_required"
        );
        assert_eq!(
            all["launch_refusals"][1]["preparation_failure"],
            "usage_limited"
        );
        let mut mixed = json!({"status":"pending","checkpoint":{"nodes":[{"attempts":[attempt("host/one", false)]},{"attempts":[attempt("host/three", true)]}]}});
        report_launch_refusals(&mut mixed, &rows);
        assert_eq!(mixed["outcome"], "partial");
        let mut fallback = json!({"status":"complete","outcome":"succeeded","checkpoint":{"nodes":[{"attempts":[attempt("host/one", false),attempt("host/three", true)]}]}});
        report_launch_refusals(&mut fallback, &rows);
        assert_eq!(fallback["outcome"], "succeeded");
        let mut failed = json!({"status":"complete","outcome":"failed","checkpoint":{"nodes":[{"state":"failed","attempts":[attempt("host/one", false)]},{"state":"succeeded","attempts":[attempt("host/three", true)]},{"state":"blocked","attempts":[]}]}});
        report_launch_refusals(&mut failed, &rows);
        assert_eq!(failed["outcome"], "failed");
    }
}

// Project durable attempt facts after the owner has tried fallback routes.
fn report_launch_refusals(result: &mut Value, rows: &[Value]) {
    let refusals: Vec<_> = rows
        .iter()
        .filter(|row| row["kind"] == "attempt" && row["settlement_reason"] == "pre_launch_refusal")
        .cloned()
        .collect();
    if refusals.is_empty() {
        return;
    }
    let Some(nodes) = result["checkpoint"]["nodes"].as_array() else {
        return;
    };
    let mut refused = 0;
    let mut admitted = 0;
    let mut other_failure = false;
    for node in nodes {
        let Some(attempt) = node["attempts"]
            .as_array()
            .and_then(|attempts| attempts.last())
        else {
            other_failure |= node["state"] == "failed" || node["state"] == "blocked";
            continue;
        };
        if attempt["launched"] == true {
            admitted += 1;
            other_failure |= node["state"] == "failed" || node["state"] == "blocked";
        } else if attempt["attempt_ref"]
            .as_str()
            .is_some_and(|id| refusals.iter().any(|row| row["attempt_id"] == id))
        {
            refused += 1;
        } else if node["state"] == "failed" || node["state"] == "blocked" {
            other_failure = true;
        }
    }
    if refused > 0
        && admitted > 0
        && !other_failure
        && matches!(result["status"].as_str(), Some("pending" | "complete"))
    {
        result["outcome"] = json!("partial");
    }
    result["launch_refusals"] = json!(refusals);
}

impl Factory {
    fn validate_host_placement(
        &self,
        node: &provenant_fabric_runtime::graph::GraphNode,
    ) -> Result<(), FacadeError> {
        if let Some(name) = node
            .attempt
            .as_ref()
            .and_then(|attempt| attempt.host.as_deref())
        {
            let local = self.context.identity.host;
            let host = if matches!(name, "local" | "localhost")
                || name
                    .parse::<provenant_core::HostId>()
                    .is_ok_and(|host| host == local)
            {
                local
            } else {
                self.context
                    .peer_host(name)
                    .ok_or(FacadeError::UnknownHost)?
            };
            if host != local
                && (node.access != provenant_fabric_runtime::graph::AccessMode::ReadOnly
                    || node.worktree.is_some()
                    || node.attempt.as_ref().is_some_and(|attempt| {
                        attempt.placement.is_some() || attempt.new_worktree.is_some()
                    }))
            {
                return Err(FacadeError::WorktreeNotApplicable);
            }
        }
        if let Some(composite) = &node.composite {
            for child in &composite.nodes {
                self.validate_host_placement(child)?;
            }
        }
        if let Some(body) = &node.loop_body {
            for child in &body.body {
                self.validate_host_placement(child)?;
            }
        }
        if let Some(fallback) = node.plan.as_ref().and_then(|plan| plan.fallback.as_ref()) {
            for child in &fallback.nodes {
                self.validate_host_placement(child)?;
            }
        }
        Ok(())
    }

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

    pub(crate) fn relay_document(
        &self,
        input: &Value,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Value, FacadeError> {
        let request =
            provenant_fabric_runtime::graph::relay::RelayOp::Add(self.child_request(input)?);
        if input["explain"] == true {
            return encode(request);
        }
        let settlement = input
            .get("wait_seconds")
            .and_then(Value::as_u64)
            .map(|seconds| {
                WaitBudget::cli(self.clock.mono_now(), seconds)
                    .map(|wait| wait.cap_at(budget.deadline().as_mono_time()))
            })
            .transpose()
            .map_err(|_| FacadeError::InvalidInput { detail: None })?;
        let launch = provenant_cli::fabric::graph_relay::writer_launch_budget(
            &self.context,
            &self.clock,
            budget,
        );
        let within =
            std::time::Duration::from_micros(launch.remaining(self.clock.mono_now()).as_micros())
                .saturating_add(std::time::Duration::from_secs(1));
        let receipt = provenant_cli::fabric::graph_relay::from_lane(&request, cancel, within)
            .map_err(relay_error)?;
        if let Some(settlement) = settlement {
            let remaining = settlement.remaining(self.clock.mono_now()).as_micros();
            if remaining == 0 {
                return Ok(json!({"receipt":receipt,"observation_error":"child wait timed out"}));
            }
            return Ok(
                match provenant_cli::fabric::graph_relay::wait_for_children(
                    cancel,
                    std::time::Duration::from_micros(remaining),
                ) {
                    Ok(children) => json!({"receipt":receipt,"children":children}),
                    Err(error) => json!({"receipt":receipt,"observation_error":error.to_string()}),
                },
            );
        }
        Ok(receipt)
    }

    pub(crate) fn child_request(
        &self,
        input: &Value,
    ) -> Result<provenant_fabric_runtime::graph::relay::ChildRequest, FacadeError> {
        use provenant_fabric_runtime::graph::relay::{ChildLaunch, ChildRequest};
        let input = launch_input(&run_envelope(input)?)?;
        let input = &input;
        let invalid = || FacadeError::invalid(InvalidInputDetail::Field("document".into()));
        let raw = input.get("document").filter(|value| !value.is_null());
        if raw.is_some_and(|value| !value.is_object()) {
            return Err(invalid());
        }
        // ChildLaunch carries nodes and templates, not graph-wide controls.
        if let Some(document) = raw {
            provenant_fabric_runtime::graph::template::validate_document_schema_version(document)
                .map_err(|_| {
                FacadeError::invalid(InvalidInputDetail::Field("schema_version".into()))
            })?;
            for field in [
                "integration_branch",
                "limits",
                "delivery",
                "review_base",
                "decision_session",
                "retry",
                "exclude",
                "network",
                "capability",
                "no_capability",
            ] {
                if document.get(field).is_some() {
                    return Err(FacadeError::invalid(InvalidInputDetail::Field(
                        field.into(),
                    )));
                }
            }
        }
        // Distinct calls create distinct children; callers opt into replay with a key.
        let idempotency_key = match input
            .get("operation_id")
            .filter(|value| !value.is_null())
            .or_else(|| input.get("idempotency_key"))
            .or_else(|| raw.and_then(|document| document.get("idempotency_key")))
        {
            Some(value) => value
                .as_str()
                .filter(|key| !key.is_empty() && key.len() <= 200)
                .ok_or_else(invalid)?
                .to_owned(),
            None => provenant_core::hex(
                &provenant_os::fs::entropy_128().map_err(|_| FacadeError::StateCorrupt)?,
            ),
        };
        let digest =
            provenant_records::CanonicalV1::file_digest(idempotency_key.as_bytes()).to_string();
        let key = format!("child-{}", &digest[7..39]);
        let detached = input
            .get("detached")
            .or_else(|| raw.and_then(|document| document.get("detached")))
            .map(|value| value.as_bool().ok_or_else(invalid))
            .transpose()?
            .unwrap_or(false);
        // These controls belong to the relay envelope, not the template schema.
        let mut launch_document = raw.cloned();
        if let Some(fields) = launch_document.as_mut().and_then(Value::as_object_mut) {
            fields.remove("idempotency_key");
            fields.remove("detached");
        }
        let raw = launch_document.as_ref();
        let has_overrides = ["brief", "use", "tier", "over"]
            .iter()
            .any(|field| input.get(*field).is_some_and(|value| !value.is_null()))
            || input
                .get("set")
                .is_some_and(|value| value.as_object().is_none_or(|object| !object.is_empty()));
        let full_document =
            raw.filter(|document| document.get("graph_id").is_some() && !has_overrides);
        let mut inline = raw
            .and_then(|document| document.get("templates"))
            .cloned()
            .unwrap_or_else(|| json!({}));
        let mut node = if let Some(document) = full_document {
            json!({"access":"read_only", "composite":{"nodes":document["nodes"], "exports":[]}})
        } else {
            match raw {
                Some(document) if document.get("use").is_some() => {
                    let mut node = document.clone();
                    provenant_fabric_runtime::graph::template::separate_document_fields(&mut node)
                        .map_err(|_| {
                            FacadeError::invalid(InvalidInputDetail::Field("schema_version".into()))
                        })?;
                    node
                }
                Some(document) => {
                    inline["lane-document"] = document.clone();
                    json!({"use":"lane-document"})
                }
                None => json!({"use":"task"}),
            }
        };
        let library = self.template_library(Some(&inline))?;
        if full_document.is_none() {
            for field in ["use", "brief", "tier", "over"] {
                if let Some(value) = input.get(field).filter(|value| !value.is_null()) {
                    node[field] = value.clone();
                }
            }
            let template = library
                .resolve(node["use"].as_str().unwrap_or("task"))
                .map_err(|_| invalid())?;
            let mut parameters = node.get("with").cloned().unwrap_or_else(|| json!({}));
            if let Some(overrides) = input.get("set").filter(|value| !value.is_null()) {
                parameters
                    .as_object_mut()
                    .ok_or_else(invalid)?
                    .extend(overrides.as_object().ok_or_else(invalid)?.clone());
            }
            if template.parameters.contains_key("brief")
                && let Some(brief) = input.get("brief").filter(|value| !value.is_null())
            {
                parameters["brief"] = brief.clone();
            }
            if template.is_composite() {
                let fields = node.as_object_mut().ok_or_else(invalid)?;
                fields.remove("brief");
                fields.remove("tier");
            }
            node["with"] = parameters;
        }
        bind_launch_controls(input, &mut node, &self.cwd)?;
        node["id"] = json!(key);
        let resolvers = self.launch_resolvers(raw)?;
        let mut compiled = provenant_fabric_runtime::graph::template::Compiler {
            library: &library,
            resolvers: &resolvers,
        }
        .lower(&node)
        .map_err(compile_error)?;
        if let Some(tier) = input.get("tier").filter(|value| !value.is_null()) {
            let tier = tier.as_str().ok_or_else(invalid)?;
            let route = serde_json::from_value(json!({"alias":tier})).map_err(|_| invalid())?;
            let mut typed = serde_json::from_value(compiled).map_err(|_| invalid())?;
            override_tier(&mut typed, &route);
            compiled = encode(typed)?;
        }
        let mut typed = serde_json::from_value(compiled).map_err(|_| invalid())?;
        override_launch(&mut typed, input)?;
        self.validate_host_placement(&typed)?;
        compiled = encode(typed)?;
        // The relay is bound to the parent's graph, while the selected checkout
        // is an ordinary child launch input. Keep it on every provider leaf.
        fn bind_directory(node: &mut Value, directory: &Path) {
            let mut container = false;
            for pointer in ["/composite/nodes", "/loop/body"] {
                if let Some(children) = node.pointer_mut(pointer).and_then(Value::as_array_mut) {
                    container = true;
                    for child in children {
                        bind_directory(child, directory);
                    }
                }
            }
            if !container
                && (node.get("kind").is_none()
                    || matches!(node["kind"].as_str(), Some("task" | "plan" | "synthesise")))
            {
                if node.get("attempt").is_none_or(Value::is_null) {
                    node["attempt"] = json!({});
                }
                node["attempt"]["directory"] = json!(directory);
            }
        }
        bind_directory(&mut compiled, &self.cwd);
        let writer = compiled["access"] == "worktree_write"
            && compiled.get("composite").is_none()
            && compiled.get("loop").is_none();
        let write_scope = if writer {
            serde_json::from_value(
                compiled
                    .get("write_scope")
                    .cloned()
                    .unwrap_or_else(|| json!([])),
            )
            .map_err(|_| invalid())?
        } else {
            Vec::new()
        };
        Ok(ChildRequest {
            id: key.clone(),
            template: node["use"].as_str().unwrap_or("task").into(),
            brief: input
                .get("brief")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            tier: input.get("tier").and_then(Value::as_str).map(str::to_owned),
            timeout_seconds: None,
            writer,
            write_scope,
            detached,
            idempotency_key,
            reason: Some("run".into()),
            launch: Some(ChildLaunch {
                node: compiled,
                templates: serde_json::from_value(inline).map_err(|_| invalid())?,
            }),
        })
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
            .map_err(|_| FacadeError::invalid(InvalidInputDetail::TemplateInvalid))?;
        library
            .load_dir(Layer::Repository, &self.cwd.join(".provenant/templates"))
            .map_err(|_| FacadeError::invalid(InvalidInputDetail::TemplateInvalid))?;
        if let Some(inline) = inline.filter(|value| !value.is_null()) {
            let templates: BTreeMap<String, Template> = serde_json::from_value(inline.clone())
                .map_err(|_| FacadeError::invalid(InvalidInputDetail::TemplateInvalid))?;
            library = library
                .with(Layer::Inline, &templates)
                .map_err(|_| FacadeError::invalid(InvalidInputDetail::TemplateInvalid))?;
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
            if matches!(operation, "status" | "output" | "trace") {
                let op = provenant_fabric_runtime::graph::relay::RelayOp::Read {
                    input: input.clone(),
                };
                return provenant_cli::fabric::graph_relay::from_lane(
                    &op,
                    cancel,
                    std::time::Duration::from_micros(
                        budget.remaining(self.clock.mono_now()).as_micros(),
                    ),
                )
                .map_err(relay_error);
            }
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
                    .map_err(relay_error);
            }
            if operation == "stop" {
                let op = provenant_fabric_runtime::graph::relay::RelayOp::Stop {
                    node: required(&input, "id")?.to_owned(),
                };
                let within = std::time::Duration::from_micros(
                    budget.remaining(self.clock.mono_now()).as_micros(),
                );
                return provenant_cli::fabric::graph_relay::from_lane(&op, cancel, within)
                    .map_err(relay_error);
            }
            if operation == "retry" {
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
            "run" => {
                if input["detached"] == true {
                    return Err(FacadeError::invalid(InvalidInputDetail::Field(
                        "detached".into(),
                    )));
                }
                self.submit_document(&input, budget, cancel)
            }
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
        let input = launch_input(&run_envelope(input)?)?;
        let input = &input;
        let operation = input.get("operation_id").and_then(Value::as_str);
        if let Some(operation) = operation
            && input["explain"] != true
            && input.pointer("/document/graph_id").is_none()
        {
            let id = provenant_fabric_runtime::graph::template::SubmissionIdentity::Fresh
                .graph_id(&Value::Null, "", Some(operation))
                .map_err(graph_input_reason)?;
            if FileJournal::read_checkpoint(
                &self.context.roots.state,
                self.context.identity.project,
                &id,
            )
            .map_err(graph_journal_error)?
            .is_some()
            {
                let execution = FileJournal::execution(
                    &self.context.roots.state,
                    self.context.identity.project,
                    &id,
                )
                .map_err(graph_journal_error)?
                .ok_or(FacadeError::StateCorrupt)?;
                return self.launch_submission(execution.document, input, budget, cancel);
            }
        }
        if input["explain"] != true {
            crate::install::cutover_admission_guard(&self.context.roots.state)?;
        }
        let raw = input.get("document");
        if let Some(document) = raw {
            provenant_fabric_runtime::graph::template::validate_document_schema_version(document)
                .map_err(|_| {
                FacadeError::invalid(InvalidInputDetail::Field("schema_version".into()))
            })?;
        }
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
        let has_overrides = [
            "brief",
            "use",
            "tier",
            "over",
            "access",
            "worktree",
            "branch",
            "base",
            "write_scope",
        ]
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
            let mut template = document.clone();
            provenant_fabric_runtime::graph::template::separate_document_fields(&mut template)
                .map_err(|_| {
                    FacadeError::invalid(InvalidInputDetail::Field("schema_version".into()))
                })?;
            inline["lane-document"] = template;
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
        let template_name = input["use"]
            .as_str()
            .or_else(|| lane_document.and_then(|document| document["use"].as_str()))
            .unwrap_or(if lane_document.is_some() {
                "lane-document"
            } else {
                "task"
            });
        let policy = library
            .resolve(template_name)
            .map_err(graph_input_reason)?
            .identity
            .unwrap_or_default();
        let mut submission = input.clone();
        if let Some(fields) = submission.as_object_mut() {
            fields.remove("explain");
            fields.remove("wait_seconds");
        }
        let mut identity = json!({
            "submission": submission,
            "directory": self.cwd,
            "templates": library.snapshot(),
            "resolvers": resolver,
        });
        // Review ranges are launch controls: resolve them on the host before
        // admitting a graph, and bind immutable object IDs into its identity.
        let review_launch = template_name == "review" || template_name.starts_with("review.");
        let setting = |name: &str| -> Result<Option<&str>, FacadeError> {
            match input.get("set").and_then(|set| set.get(name)) {
                None => Ok(None),
                Some(value) => value
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(Some)
                    .ok_or_else(|| FacadeError::invalid(InvalidInputDetail::Field(name.into()))),
            }
        };
        let subject = if review_launch {
            setting("subject")?
        } else {
            None
        };
        let review_base = if review_launch {
            setting("review_base")?
        } else {
            None
        };
        if review_launch && subject.is_some() != review_base.is_some() {
            return Err(FacadeError::invalid(InvalidInputDetail::Field(
                "review requires both subject and review_base".into(),
            )));
        }
        let mut resolved_head = None;
        let mut resolved_review_base = None;
        if let (Some(subject), Some(base)) = (subject, review_base) {
            let git = provenant_os::git::git_executable()
                .ok_or(FacadeError::InvalidInput { detail: None })?;
            let head =
                provenant_run::resolve_git_base(&git, &self.cwd, subject).map_err(run_error)?;
            let base = provenant_run::resolve_git_base(&git, &self.cwd, base).map_err(run_error)?;
            identity["head"] = json!(head);
            identity["review_base"] = json!(base);
            resolved_head = Some(head);
            resolved_review_base = Some(base);
        }
        if resolved_head.is_none()
            && policy == provenant_fabric_runtime::graph::template::SubmissionIdentity::Content
            && raw.and_then(|document| document.get("graph_id")).is_none()
        {
            let git = provenant_os::git::git_executable()
                .ok_or(FacadeError::InvalidInput { detail: None })?;
            let head =
                provenant_run::resolve_git_base(&git, &self.cwd, "HEAD").map_err(run_error)?;
            identity["head"] = json!(head);
            resolved_head = Some(head);
        }
        let nonce = provenant_fabric_runtime::coordination::generated_operation_id("graph")
            .map_err(|_| FacadeError::StateCorrupt)?;
        let generated_id = policy
            .graph_id(&identity, &nonce, operation)
            .map_err(graph_input_reason)?;
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
                    nodes: decode_input(raw["nodes"].clone(), "nodes")?,
                    ..Default::default()
                };
                patch::submit(id, &patch, &context, raw["integration_branch"].as_str())
                    .map_err(graph_patch_error)?
            } else {
                decode_input::<GraphDocument>(raw.clone(), "document")?
            }
        } else {
            let mut node = match lane_document {
                Some(document) if document.get("use").is_some() => {
                    let mut node = document.clone();
                    provenant_fabric_runtime::graph::template::separate_document_fields(&mut node)
                        .map_err(|_| {
                            FacadeError::invalid(InvalidInputDetail::Field("schema_version".into()))
                        })?;
                    node
                }
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
                .map_err(graph_input_reason)?;
            if template.is_composite()
                && ["access", "worktree", "branch", "base", "write_scope"]
                    .iter()
                    .any(|field| input.get(*field).is_some_and(|value| !value.is_null()))
            {
                return Err(FacadeError::invalid(InvalidInputDetail::Field(
                    "launch controls require a leaf template".into(),
                )));
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
            if review_launch && let Some(parameters) = parameters.as_object_mut() {
                parameters.remove("subject");
                parameters.remove("review_base");
            }
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
            .map_err(graph_patch_error)?
        };
        if review_launch {
            if let Some(base) = resolved_review_base {
                document.review_base = Some(base);
                document.schema_version = document.schema_version.max(2);
            } else {
                document
                    .extra
                    .insert("review_subject_missing".into(), json!(true));
            }
        }
        document
            .extra
            .insert("template_layers".into(), json!(library.snapshot_layers()));
        document
            .extra
            .insert("template_library".into(), json!(library.snapshot()));
        document
            .extra
            .insert("template_resolvers".into(), json!(resolver));
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
        for node in &mut document.nodes {
            override_launch(node, input)?;
            self.validate_host_placement(node)?;
        }
        if let Some(head) = &resolved_head {
            for node in &mut document.nodes {
                bind_submission_subject(node, head, review_launch && subject.is_none());
            }
            document.schema_version = document.schema_version.max(u64::from(
                provenant_fabric_runtime::graph::minimum_schema_version(&document),
            ));
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
        .map_err(graph_patch_error)?;
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
        self.launch_submission(document, input, budget, cancel)
    }

    fn launch_submission(
        &self,
        document: GraphDocument,
        input: &Value,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Value, FacadeError> {
        let scratch = self
            .prepare_scratch(cancel, budget)
            .map_err(scratch_prepare_error)?;
        let path = publish_graph_document(&scratch, &encode(&document)?)
            .map_err(|_| FacadeError::InvalidInput { detail: None })?;
        let run = self.graph_with_scratch(
            GraphInput::Run {
                extra: Default::default(),
                document: path.clone(),
            },
            budget,
            cancel,
            Some(&scratch),
            input
                .get("wait_seconds")
                .and_then(Value::as_u64)
                .map(|_| budget),
        )?;
        let run = self.wait_document(run, input, budget, cancel, &path, &scratch)?;
        let timed_out = input.get("wait_seconds").and_then(Value::as_u64).is_some()
            && run.status == GraphStatus::Pending
            && !cancel.is_cancelled();
        let target = super::unified::GraphTarget {
            graph: run.checkpoint.graph_id.clone(),
            node: None,
            attempt: None,
        };
        let mut rows = super::unified::status_rows(&run, &target);
        rows.retain(|row| {
            row["kind"] == "attempt"
                && run
                    .checkpoint
                    .nodes
                    .iter()
                    .flat_map(|node| &node.attempts)
                    .any(|attempt| {
                        !attempt.launched
                            && attempt.state
                                == provenant_fabric_runtime::graph::AttemptState::Terminal
                            && attempt.attempt_ref.as_deref() == row["attempt_id"].as_str()
                    })
        });
        if !rows.is_empty() {
            super::unified::enrich_status_rows_within(
                &mut rows,
                &self.context,
                &self.clock,
                cancel,
                budget,
            )?;
        }
        let mut result = encode(run)?;
        report_launch_refusals(&mut result, &rows);
        result["timed_out"] = json!(timed_out);
        Ok(result)
    }

    fn edit_document(
        &self,
        input: &Value,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Value, FacadeError> {
        crate::install::cutover_admission_guard(&self.context.roots.state)?;
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
        .map_err(graph_input_reason)?;
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
        crate::install::cutover_admission_guard(&self.context.roots.state)?;
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
            patch::retry_node_and_dependants(
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
            .map_err(retry_patch_error)?;
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
        super::unified::enrich_status_rows_within(
            &mut rows,
            &self.context,
            &self.clock,
            cancel,
            budget,
        )?;
        Ok(json!({"rows":rows,"timed_out":timed_out}))
    }

    fn wait_document(
        &self,
        run: provenant_fabric_runtime::graph::GraphRun,
        input: &Value,
        budget: WaitBudget,
        cancel: &CancelToken,
        document: &Path,
        scratch: &Path,
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
        wait_submission(run, wait, &self.clock, &self.backoff, cancel, |id| {
            let observed = self.graph(
                GraphInput::Status {
                    extra: Default::default(),
                    id: id.to_owned(),
                },
                budget,
                cancel,
            )?;
            // A waited submission owns admission progress. Status itself remains
            // read-only; only a due launch retry is advanced under the journal lock.
            let now = self.clock.wall_now().ok();
            let due = observed.checkpoint.nodes.iter().any(|node| {
                node.launch_retry
                    .as_ref()
                    .and_then(|retry| retry.wake_at)
                    .zip(now)
                    .is_some_and(|(wake, now)| now >= wake)
            });
            if !due || budget.remaining(self.clock.mono_now()).as_micros() == 0 {
                return Ok(observed);
            }
            self.graph_with_scratch(
                GraphInput::Run {
                    extra: Default::default(),
                    document: document.to_owned(),
                },
                budget,
                cancel,
                Some(scratch),
                None,
            )
        })
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
            events.retain(|event| event.subject == node);
        }
        Ok(json!({"graph_id":id,"events":events}))
    }

    fn graph_stats(&self, input: &Value) -> Result<Value, FacadeError> {
        let by = input.get("by").and_then(Value::as_str).unwrap_or("tier");
        if !matches!(by, "node" | "tier" | "template") {
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
            let subject = target.as_ref().and_then(|target| target.node.as_ref());
            let selected_attempt = target.as_ref().and_then(|target| target.attempt.as_ref());
            for event in FileJournal::read_events(
                &self.context.roots.state,
                self.context.identity.project,
                &id,
            )
            .map_err(graph_journal_error)?
            {
                if subject
                    .as_ref()
                    .is_some_and(|subject| subject.as_str() != event.subject)
                {
                    continue;
                }
                if let Some(attempt) = event.attempt {
                    if selected_attempt
                        .as_ref()
                        .is_some_and(|selected| selected.as_str() != attempt.identity)
                    {
                        continue;
                    }
                    attempts.insert(
                        (id.clone(), attempt.identity.clone()),
                        (event.subject, attempt),
                    );
                }
            }
        }
        let mut groups = BTreeMap::<String, Value>::new();
        for ((graph, _), (node, attempt)) in attempts {
            let group = match by {
                "node" => format!("{graph}/{node}"),
                "template" => attempt.template.clone(),
                _ => attempt.tier.clone(),
            };
            let row = groups.entry(group.clone()).or_insert_with(|| {
                json!({
                    "name":group,"attempts":0,"tokens":0,"cost_micro_usd":0,
                    "wall_time_seconds":0,"queue_time_ms":0,"cache_read_tokens":0,
                    "input_tokens":0,"unknown_usage":0,"outcomes":{},
                    "unknown_queue_time":0,"unknown_cache_usage":0
                })
            });
            add(row, "attempts", 1);
            for (key, value) in [
                ("tokens", attempt.tokens),
                ("cost_micro_usd", attempt.cost_micro_usd),
                ("wall_time_seconds", attempt.wall_time_seconds),
                ("queue_time_ms", attempt.queue_time_ms),
            ] {
                if let Some(value) = value {
                    add(row, key, value);
                }
            }
            // The share uses input tokens only, with cache writes counted once.
            if let (Some(read), Some(input)) = (attempt.cache_read_tokens, attempt.input_tokens) {
                add(row, "cache_read_tokens", read);
                add(row, "input_tokens", input);
            } else {
                add(row, "unknown_cache_usage", 1);
            }
            if attempt.queue_time_ms.is_none() {
                add(row, "unknown_queue_time", 1);
            }
            add(
                &mut row["outcomes"],
                attempt.outcome.as_deref().unwrap_or("unsettled"),
                1,
            );
            if !attempt.has_usage {
                add(row, "unknown_usage", 1);
            }
        }
        for row in groups.values_mut() {
            let input = row["input_tokens"].as_u64().unwrap_or(0);
            row["cache_read_share"] = if input == 0 {
                Value::Null
            } else {
                json!(row["cache_read_tokens"].as_u64().unwrap_or(0) as f64 / input as f64)
            };
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
    #[test]
    fn decode_errors_name_schema_paths_without_keys_or_values() -> Result<()> {
        use provenant_fabric_runtime::graph::GraphDocument;
        let node = |field: &str, value: Value| {
            let mut node = json!({"id":"one","access":"read_only","prompt":"p","route":{"adapter":"mock","model":"m"}});
            match field {
                "role" => node["role"] = value,
                other => node["attempt"] = json!({ other: value }),
            }
            json!({"schema_version":1,"graph_id":"g","nodes":[node]})
        };
        for (field, path) in [
            ("directory", "nodes[0].attempt.directory"),
            ("new_worktree", "nodes[0].attempt.new_worktree"),
            ("role", "nodes[0].role"),
        ] {
            let error = super::decode_input::<GraphDocument>(node(field, json!(7)), "")
                .expect_err("decode must fail");
            let text = error.diagnostic().detail.unwrap_or_default();
            assert!(text.contains(path), "{text}");
            assert!(text.contains("invalid type or value"), "{text}");
        }
        let secret = "ghp_TOKEN_SENTINEL";
        let mut doc = node("role", json!("fine"));
        doc["nodes"][0]["access"] = json!(secret);
        let error = super::decode_input::<GraphDocument>(doc, "").expect_err("decode must fail");
        let wire = serde_json::to_string(&error).unwrap();
        assert!(!wire.contains(secret), "{wire}");
        assert!(wire.contains("nodes[0].access"), "{wire}");
        // Map keys never appear, whatever characters they contain.
        for key in ["foo.secret", "k[0]secret", "userkey"] {
            #[derive(Debug, serde::Deserialize)]
            #[allow(dead_code, reason = "decoded only to provoke a typed map failure")]
            struct Doc {
                nodes: Vec<Node>,
            }
            #[derive(Debug, serde::Deserialize)]
            #[allow(dead_code, reason = "decoded only to provoke a typed map failure")]
            struct Node {
                attempt: Attempt,
            }
            #[derive(Debug, serde::Deserialize)]
            #[allow(dead_code, reason = "decoded only to provoke a typed map failure")]
            struct Attempt {
                extra: BTreeMap<String, u8>,
            }
            let doc = json!({"nodes":[{"attempt":{"extra":{key: "not a number"}}}]});
            let error = super::decode_input::<Doc>(doc, "").expect_err("decode must fail");
            let wire = serde_json::to_string(&error).unwrap();
            assert!(
                !wire.contains("secret") && !wire.contains("userkey"),
                "{wire}"
            );
            let back: FacadeError = serde_json::from_str(&wire).unwrap();
            assert_eq!(back, error);
            assert_eq!(serde_json::to_string(&back).unwrap(), wire);
            let text = back.diagnostic().detail.unwrap_or_default();
            assert!(text.contains("nodes[0].attempt.extra`"), "{text}");
        }
        Ok(())
    }

    #[test]
    fn relay_refusals_preserve_named_code_and_detail() -> Result<()> {
        for (code, detail) in [
            ("parent_settled", "parent has already settled"),
            ("child_depth", "child exceeds limits.child_depth=2"),
        ] {
            let error = super::relay_error(ServiceError::Cli {
                code: code.into(),
                message: detail.into(),
                data: Value::Null,
            });
            assert_eq!(error.code(), code);
            assert!(matches!(
                &error,
                FacadeError::GraphRelayRefused {
                    detail: Some(InvalidInputDetail::GraphReason(reason)), ..
                } if reason.contains(detail)
            ));
            let diagnostic = error.diagnostic();
            assert_eq!(diagnostic.code, code);
            assert_eq!(
                diagnostic.detail.as_deref(),
                Some("graph input could not be processed")
            );
            let restored: FacadeError = serde_json::from_value(serde_json::to_value(&error)?)?;
            assert_eq!(restored.code(), code);
            assert_eq!(restored.diagnostic(), diagnostic);
            let mcp = ServiceError::Facade(error).structured_error();
            assert_eq!(mcp["code"], code);
            assert!(!mcp["message"].as_str().unwrap_or_default().contains(detail));
        }
        Ok(())
    }

    #[test]
    fn relay_upgrade_refusal_retains_safe_drain_guidance() -> Result<()> {
        let error = super::relay_error(ServiceError::Cli {
            code: "UPGRADE_DRAIN_REQUIRED".into(),
            message: "PROMPT_SENTINEL private source text".into(),
            data: json!({"private": "PROMPT_SENTINEL"}),
        });
        let wire = serde_json::to_value(&error)?;
        assert!(!serde_json::to_string(&wire)?.contains("PROMPT_SENTINEL"));
        let restored: FacadeError = serde_json::from_value(wire)?;
        assert_eq!(restored, error);
        let diagnostic = restored.diagnostic();
        assert_eq!(diagnostic.code, "UPGRADE_DRAIN_REQUIRED");
        assert_eq!(
            diagnostic.detail.as_deref(),
            Some(
                "Finish or cancel old pending graphs and drain live lanes with the previous release before admitting new work."
            )
        );
        assert!(
            diagnostic
                .next
                .as_deref()
                .ok_or("drain repair")?
                .contains("--yes cannot bypass")
        );
        let mcp = ServiceError::Facade(restored).structured_error();
        assert_eq!(mcp["code"], "UPGRADE_DRAIN_REQUIRED");
        assert!(
            mcp["message"]
                .as_str()
                .ok_or("drain message")?
                .contains("previous release")
        );
        assert!(!serde_json::to_string(&mcp)?.contains("PROMPT_SENTINEL"));
        Ok(())
    }

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
    fn child_unknown_parameter_keeps_the_compiler_refusal() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let error = factory
            .child_request(&json!({
                "op":"run", "brief":"Inspect", "set":{"bogus":true}
            }))
            .expect_err("task has no bogus parameter");
        let FacadeError::InvalidInput {
            detail: Some(InvalidInputDetail::Parameter(refusal)),
        } = &error
        else {
            return Err("missing parameter refusal".into());
        };
        assert_eq!(refusal.template, "task");
        assert!(
            matches!(&refusal.fault, provenant_fabric_runtime::graph::template::ParameterFault::Unknown { key: Some(key), .. } if key == "bogus"),
            "{refusal:?}"
        );
        let diagnostic = error.diagnostic();
        assert_eq!(diagnostic.code, "invalid_input");
        let detail = diagnostic.detail.as_deref().unwrap_or_default();
        assert!(
            detail.contains("template task has no parameter bogus"),
            "{detail}"
        );
        assert!(detail.contains("accepted parameters:"), "{detail}");
        let restored: FacadeError = serde_json::from_value(serde_json::to_value(&error)?)?;
        assert_eq!(restored.diagnostic(), diagnostic);
        Ok(())
    }

    #[test]
    fn branch_without_base_uses_the_remote_default_branch() -> Result {
        let mut node = json!({"use":"task"});
        bind_launch_controls(&json!({"branch":"topic"}), &mut node, Path::new("/repo"))?;
        assert_eq!(
            node["attempt"]["new_worktree"]["base"],
            "refs/remotes/origin/HEAD"
        );
        assert_eq!(node["attempt"]["new_worktree"]["default_base"], true);
        let mut explicit = json!({"use":"task"});
        bind_launch_controls(
            &json!({"branch":"topic", "base":"refs/remotes/origin/HEAD"}),
            &mut explicit,
            Path::new("/repo"),
        )?;
        assert!(
            explicit["attempt"]["new_worktree"]
                .get("default_base")
                .is_none()
        );
        assert!(
            bind_launch_controls(&json!({"base":"HEAD"}), &mut node, Path::new("/repo")).is_err()
        );
        Ok(())
    }

    #[test]
    fn child_operation_identity_replays_the_same_launch() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let input = json!({"op":"run", "brief":"Inspect", "operation_id":"child-op"});
        let first = factory.child_request(&input)?;
        let replay = factory.child_request(&input)?;
        assert_eq!(first.idempotency_key, "child-op");
        assert_eq!(first.id, replay.id);
        assert_eq!(first.launch, replay.launch);
        assert!(
            factory
                .child_request(&json!({"op":"run", "operation_id":"invalid op"}))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn submission_identity_tracks_template_content_head_and_directory() -> Result {
        let rig = crate::worktree_tests::repository()?;
        let factory = factory(&rig)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let explain = |factory: &Factory, input: Value| -> Result<String> {
            let label = input.to_string();
            let response = factory
                .graph_document(input, budget, &cancel)
                .map_err(|error| format!("{label}: {error:?}"))?;
            Ok(response["document"]["graph_id"]
                .as_str()
                .ok_or("graph id")?
                .to_owned())
        };
        let task = json!({"op":"run","brief":"same brief","explain":true});
        assert_ne!(
            explain(&factory, task.clone())?,
            explain(&factory, task.clone())?
        );
        let inline = json!({"op":"run","document":{"identity":"content","brief":"inline task","tier":"senior"},"explain":true});
        assert_eq!(
            explain(&factory, inline.clone())?,
            explain(&factory, inline)?
        );
        // Content identity includes launch policy, even when the brief and
        // pinned repository subject are unchanged.
        let controlled = json!({
            "op":"run", "explain":true,
            "document":{"identity":"content", "brief":"Inspect launch policy",
                "tier":"senior", "access":"read_only"},
            "attempt":{"host":"local", "confidential":true, "provider_timeout":42}
        });
        let original = explain(&factory, controlled.clone())?;
        assert_eq!(original, explain(&factory, controlled.clone())?);
        for (field, value) in [
            ("host", json!("localhost")),
            ("confidential", json!(false)),
            ("provider_timeout", json!(43)),
        ] {
            let mut changed = controlled.clone();
            changed["attempt"][field] = value;
            let expected: provenant_fabric_runtime::graph::AttemptSpec =
                serde_json::from_value(changed["attempt"].clone())?;
            let response = factory.graph_document(changed.clone(), budget, &cancel)?;
            let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
            assert_ne!(document.graph_id, original, "attempt.{field}");
            assert_eq!(
                document.graph_id,
                explain(&factory, changed)?,
                "attempt.{field}"
            );
            let expanded = document.expanded(&factory.context.config.graph.limits)?;
            assert_eq!(
                expanded.nodes[0].attempt.as_ref(),
                Some(&expected),
                "attempt.{field}"
            );
        }
        let review = factory.graph_document(
            json!({"op":"run","use":"review","brief":"Review","explain":true}),
            budget,
            &cancel,
        )?;
        assert_eq!(
            review["document"]["nodes"][0]["source"],
            json!({
                "type":"commit", "commit":crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?
            })
        );
        assert_eq!(review["document"]["review_subject_missing"], true);
        assert!(
            review["document"]["nodes"][0]["prompt"]
                .as_str()
                .unwrap()
                .contains("No review subject was supplied")
        );
        let base = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
        crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "review subject"])?;
        let head = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
        let pinned = factory.graph_document(
            json!({"op":"run","use":"review","brief":"Review","set":{"subject":head,"review_base":base},"explain":true}), budget, &cancel,
        )?;
        assert_eq!(pinned["document"]["review_base"], base);
        assert_eq!(pinned["document"]["nodes"][0]["source"]["commit"], head);
        let error = factory.graph_document(json!({"op":"run","use":"review","set":{"subject":"missing-ref","review_base":base},"explain":true}), budget, &cancel).unwrap_err();
        assert!(
            matches!(error, FacadeError::InvalidInput { detail: Some(InvalidInputDetail::UnresolvableBase(ref name)) } if name == "missing-ref")
        );
        for name in ["review", "review.security", "deliver"] {
            let mut input = json!({"op":"run","use":name,"brief":"same brief","explain":true});
            if name == "deliver" {
                input["set"] = json!({"worktree":rig.project,"write_scope":["src"]});
            }
            let first = explain(&factory, input.clone())?;
            assert_eq!(first, explain(&factory, input.clone())?);
            crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "new subject"])?;
            assert_ne!(first, explain(&factory, input.clone())?);
            let mut changed = input.clone();
            changed["brief"] = json!("changed brief");
            assert_ne!(
                explain(&factory, input.clone())?,
                explain(&factory, changed)?
            );
            let mut elsewhere = factory.for_graph_directory(&rig.project)?;
            let nested = rig.project.join("nested");
            std::fs::create_dir_all(&nested)?;
            elsewhere.cwd = nested;
            assert_ne!(
                explain(&factory, input.clone())?,
                explain(&elsewhere, input)?
            );
        }
        let nested = json!({"op":"run","document":{"use":"task","brief":"nested","operation_id":"nested-replay"},"explain":true});
        assert_eq!(
            explain(&factory, nested.clone())?,
            explain(&factory, nested)?
        );
        let mut replay = task;
        replay["operation_id"] = json!("repeat-task");
        let first = explain(&factory, replay.clone())?;
        crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "later subject"])?;
        assert_eq!(first, explain(&factory, replay.clone())?);
        replay["operation_id"] = json!("another-task");
        assert_ne!(first, explain(&factory, replay)?);
        Ok(())
    }

    #[test]
    fn reviews_of_the_same_subject_attach_to_the_completed_attempt() -> Result {
        use provenant_fabric_runtime::graph::{self, AttemptOwner, GraphExecution};
        struct Reviewer(usize);
        impl AttemptOwner for Reviewer {
            type Error = std::convert::Infallible;
            fn actor(&self) -> String {
                "review-fixture".into()
            }
            fn observe_or_start(
                &mut self,
                _: &graph::AttemptKey,
                _: graph::NodeInput<'_>,
                _: &graph::GraphRoute,
                _: graph::AccessMode,
                _: Option<&str>,
                _: graph::Replay<'_>,
            ) -> std::result::Result<graph::AttemptObservation, Self::Error> {
                self.0 += 1;
                Ok(graph::AttemptObservation::Terminal {
                    attempt_ref: Some("fixture/review".into()),
                    outcome: graph::AttemptOutcome::Succeeded,
                    launched: true,
                    result: None,
                    result_warning: None,
                    result_path: None,
                    branch: None,
                    commit: None,
                })
            }
        }
        let rig = crate::worktree_tests::repository()?;
        let factory = factory(&rig)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let input =
            json!({"op":"run", "use":"review", "brief":"Review the parser", "tier":"senior"});
        let mut preview = input.clone();
        preview["explain"] = json!(true);
        let document: GraphDocument = serde_json::from_value(
            factory.graph_document(preview, budget, &cancel)?["document"].clone(),
        )?;
        let mut reviewer = Reviewer(0);
        let first = {
            let mut journal = FileJournal::open(
                &factory.context.roots.state,
                factory.context.identity.project,
                &document.graph_id,
                budget,
                &cancel,
                &factory.clock,
            )?;
            journal.bind(GraphExecution {
                extra: Default::default(),
                directory: factory.cwd.clone(),
                document: document.clone(),
                review_base: None,
                integration_base: None,
            })?;
            graph::advance_with_budget(
                &document,
                &mut reviewer,
                &mut journal,
                &factory.context.config.graph.limits,
                &factory.context.config.graph.budget,
            )?
        };
        assert_eq!(first.status, GraphStatus::Complete);
        assert_eq!(reviewer.0, 1);
        // Submit through the real front door, without an operation ID. A
        // content-identical review must reuse the terminal journal rather than
        // entering the host's provider-launch path again.
        for wait in [None, Some(1)] {
            let mut repeated = input.clone();
            if let Some(seconds) = wait {
                repeated["wait_seconds"] = json!(seconds);
            }
            let attached = factory.graph_document(repeated, budget, &cancel)?;
            assert_eq!(attached["status"], "complete");
            assert_eq!(attached["timed_out"], false);
            let checkpoint: graph::GraphCheckpoint =
                serde_json::from_value(attached["checkpoint"].clone())?;
            assert_eq!(checkpoint, first.checkpoint);
            let attempts: Vec<_> = checkpoint
                .nodes
                .iter()
                .flat_map(|node| &node.attempts)
                .collect();
            assert_eq!(attempts.len(), 1);
            assert_eq!(attempts[0].attempt_ref.as_deref(), Some("fixture/review"));
        }
        assert_eq!(
            FileJournal::graph_ids(
                &factory.context.roots.state,
                factory.context.identity.project,
            )?,
            [document.graph_id]
        );
        Ok(())
    }

    #[test]
    fn explicit_operation_replays_terminal_execution_after_head_and_template_changes() -> Result {
        use provenant_fabric_runtime::graph::{self, AttemptOwner, GraphExecution};
        struct Owner;
        impl AttemptOwner for Owner {
            type Error = std::convert::Infallible;
            fn actor(&self) -> String {
                "rerun-fixture".into()
            }
            fn observe_or_start(
                &mut self,
                _: &graph::AttemptKey,
                _: graph::NodeInput<'_>,
                _: &graph::GraphRoute,
                _: graph::AccessMode,
                _: Option<&str>,
                _: graph::Replay<'_>,
            ) -> std::result::Result<graph::AttemptObservation, Self::Error> {
                Ok(graph::AttemptObservation::Terminal {
                    attempt_ref: Some("fixture/first".into()),
                    outcome: graph::AttemptOutcome::Succeeded,
                    launched: true,
                    result: None,
                    result_warning: None,
                    result_path: None,
                    branch: None,
                    commit: None,
                })
            }
        }
        let rig = crate::worktree_tests::repository()?;
        let factory = factory(&rig)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let mut input =
            json!({"op":"run","brief":"Original task","operation_id":"recover-run","explain":true});
        let document: GraphDocument = serde_json::from_value(
            factory.graph_document(input.clone(), budget, &cancel)?["document"].clone(),
        )?;
        let id = document.graph_id.clone();
        let mut journal = FileJournal::open(
            &factory.context.roots.state,
            factory.context.identity.project,
            &id,
            budget,
            &cancel,
            &factory.clock,
        )?;
        journal.bind(GraphExecution {
            extra: Default::default(),
            directory: factory.cwd.clone(),
            document: document.clone(),
            review_base: None,
            integration_base: None,
        })?;
        let first = graph::advance_with_budget(
            &document,
            &mut Owner,
            &mut journal,
            &factory.context.config.graph.limits,
            &factory.context.config.graph.budget,
        )?;
        assert_eq!(first.status, GraphStatus::Complete);
        drop(journal);
        crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "move head"])?;
        let templates = rig.project.join(".provenant/templates");
        std::fs::create_dir_all(&templates)?;
        std::fs::write(templates.join("task.toml"), "invalid template")?;
        input["explain"] = json!(false);
        let replay = factory.graph_document(input, budget, &cancel)?;
        assert_eq!(replay["checkpoint"]["graph_id"], id);
        assert_eq!(replay["status"], "complete");
        let saved = FileJournal::read_checkpoint(
            &factory.context.roots.state,
            factory.context.identity.project,
            &id,
        )?
        .ok_or("checkpoint")?;
        assert_eq!(saved.nodes[0].attempts.len(), 1);
        assert_eq!(
            saved.nodes[0].attempts[0].attempt_ref.as_deref(),
            Some("fixture/first")
        );
        Ok(())
    }

    #[test]
    fn attempt_settings_merge_before_graph_and_child_template_binding() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
        let input = json!({
            "op":"run", "explain":true, "host":"local", "account":"flag-account",
            "document":{"use":"task", "attempt":{"build_heavy":true, "provider_timeout":10}},
            "set":{
                "attempt":{"host":"overridden-peer", "account":"set-account", "confidential":true},
                "attempt.provider_timeout":42
            }
        });
        let response = factory.graph_document(input.clone(), budget, &cancel)?;
        let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
        let child = factory.child_request(&input)?;
        let child = child.launch.ok_or("child launch missing")?;
        let child: provenant_fabric_runtime::graph::GraphNode = serde_json::from_value(child.node)?;
        for node in [&document.nodes[0], &child] {
            let attempt = node.attempt.as_ref().ok_or("attempt missing")?;
            assert_eq!(attempt.host.as_deref(), Some("local"));
            assert_eq!(attempt.account.as_deref(), Some("flag-account"));
            assert!(attempt.build_heavy);
            assert!(attempt.confidential);
            assert_eq!(attempt.provider_timeout, Some(42));
        }
        // A host supplied only through --set must survive parameter extraction too.
        for settings in [
            json!({"attempt.host":"local"}),
            json!({"attempt":{"host":"local"}}),
        ] {
            let input = json!({"op":"run", "brief":"Inspect", "set":settings, "explain":true});
            let response = factory.graph_document(input.clone(), budget, &cancel)?;
            assert_eq!(response["document"]["nodes"][0]["attempt"]["host"], "local");
            let child = factory
                .child_request(&input)?
                .launch
                .ok_or("child launch missing")?;
            assert_eq!(child.node["attempt"]["host"], "local");
        }
        Ok(())
    }

    #[test]
    fn graph_and_child_launch_refuse_invalid_attempt_settings_before_publication() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
        for settings in [
            json!({"attempt":[]}),
            json!({"attempt.host":7}),
            json!({"attempt.host":""}),
            json!({"attempt.account":[]}),
            json!({"attempt.build_heavy":1}),
            json!({"attempt.confidential":"true"}),
            json!({"attempt.provider_timeout":0}),
            json!({"attempt.provider_timeout":-1}),
            json!({"attempt.provider_timeout":1.5}),
        ] {
            let input = json!({"op":"run", "brief":"Inspect", "set":settings, "explain":true});
            assert!(
                matches!(
                    factory.graph_document(input.clone(), budget, &cancel),
                    Err(FacadeError::InvalidInput { .. })
                ),
                "graph accepted {settings}"
            );
            assert!(
                matches!(
                    factory.child_request(&input),
                    Err(FacadeError::InvalidInput { .. })
                ),
                "child accepted {settings}"
            );
        }
        assert!(!rig.project.join(".provenant/scratch/graphs").exists());
        assert!(
            FileJournal::graph_ids(
                &factory.context.roots.state,
                factory.context.identity.project
            )?
            .is_empty()
        );
        Ok(())
    }

    #[test]
    fn composite_graph_and_child_apply_route_and_attempt_controls_to_every_leaf() -> Result {
        fn assert_leaves(
            node: &provenant_fabric_runtime::graph::GraphNode,
            leaves: &mut usize,
        ) -> Result {
            if let Some(composite) = &node.composite {
                for child in &composite.nodes {
                    assert_leaves(child, leaves)?;
                }
                return Ok(());
            }
            *leaves += 1;
            let route = node.route.as_ref().ok_or("leaf route missing")?;
            assert_eq!(route.adapter.as_deref(), Some("fixture"));
            assert_eq!(route.model.as_deref(), Some("fixture/model"));
            assert_eq!(route.effort.as_deref(), Some("high"));
            assert_eq!(route.account.as_deref(), Some("flag-account"));
            assert!(node.routes.is_none());
            let attempt = node.attempt.as_ref().ok_or("leaf attempt missing")?;
            assert_eq!(attempt.host.as_deref(), Some("local"));
            assert_eq!(attempt.account.as_deref(), Some("flag-account"));
            assert!(attempt.build_heavy);
            assert!(attempt.confidential);
            assert_eq!(attempt.provider_timeout, Some(42));
            Ok(())
        }
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
        let input = json!({
            "op":"run", "explain":true,
            "model":"fixture/model", "effort":"high", "account":"flag-account",
            "set":{"attempt.host":"local", "attempt.build_heavy":true,
                "attempt.confidential":true, "attempt.provider_timeout":42},
            "document":{"nodes":[
                {"id":"first", "use":"task", "brief":"Inspect first", "tier":"junior"},
                {"id":"second", "use":"task", "brief":"Inspect second", "tier":"senior"}
            ]}
        });
        let response = factory.graph_document(input.clone(), budget, &cancel)?;
        let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
        let mut graph_leaves = 0;
        for node in &document.nodes {
            assert_leaves(node, &mut graph_leaves)?;
        }
        assert_eq!(graph_leaves, 2);
        let child = factory
            .child_request(&input)?
            .launch
            .ok_or("child launch missing")?;
        let child: provenant_fabric_runtime::graph::GraphNode = serde_json::from_value(child.node)?;
        let mut child_leaves = 0;
        assert_leaves(&child, &mut child_leaves)?;
        assert_eq!(child_leaves, graph_leaves);
        Ok(())
    }

    #[test]
    fn plain_task_launch_controls_bind_writer_and_route_before_admission() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
        let response = factory.graph_document(
            json!({
                "op":"run", "brief":"Fix the parser", "tier":"senior",
                "worktree":rig.project, "write_scope":["src"], "explain":true
            }),
            budget,
            &cancel,
        )?;
        let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
        assert_eq!(document.nodes.len(), 1);
        let node = &document.nodes[0];
        assert_eq!(
            node.access,
            provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
        );
        assert_eq!(node.worktree.as_deref(), rig.project.to_str());
        assert_eq!(node.write_scope, vec!["src"]);
        assert_eq!(
            node.route.as_ref().and_then(|route| route.alias.as_deref()),
            Some("senior")
        );
        Ok(())
    }

    #[test]
    fn writer_branch_explanation_is_one_admitted_node_without_git_effects() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
        let cancel = CancelToken::new();
        let response = factory.graph_document(
            json!({
                "op":"run", "brief":"Fix the parser", "branch":"fix/parser",
                "base":"HEAD", "explain":true
            }),
            budget,
            &cancel,
        )?;
        let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
        assert_eq!(document.nodes.len(), 1);
        let node = &document.nodes[0];
        assert_eq!(node.worktree.as_deref(), Some(".worktrees/fix-parser"));
        let target = node
            .attempt
            .as_ref()
            .and_then(|spec| spec.new_worktree.as_ref())
            .ok_or("new worktree missing")?;
        assert_eq!(target.branch, "fix/parser");
        assert_eq!(target.base, "HEAD");
        assert!(!rig.project.join(".worktrees/fix-parser").exists());
        assert!(
            factory
                .graph_document(
                    json!({"op":"run","use":"deliver","worktree":rig.project,"explain":true}),
                    budget,
                    &cancel
                )
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn graph_template_errors_reach_the_agent_with_safe_diagnostics() -> Result {
        let rig = TestRig::new()?;
        // Inspection does not require a previously registered project, including
        // when the test process itself runs inside a lane.
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let mut context = Context::open_inspection(roots, &rig.home, &rig.project)?;
        context.in_lane = false;
        let factory = Factory {
            context,
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
        };
        let directory = rig.project.join(".provenant/templates");
        std::fs::create_dir_all(&directory)?;
        let path = directory.join("review.typo.toml");
        std::fs::write(&path, "accesz = \"read_only\"\n")?;
        let error = factory.template_library(None).unwrap_err();
        let structured =
            provenant_fabric_mcp::tools::ServiceError::Facade(error.clone()).structured_error();
        assert_eq!(structured["code"], error.diagnostic().code);
        assert!(
            structured["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("templates")),
            "{structured}"
        );
        assert_eq!(
            structured["next"].as_str(),
            error.diagnostic().next.as_deref()
        );
        let diagnostic = error.diagnostic();
        let text = diagnostic.detail.ok_or("agent diagnostic")?;
        assert!(
            text.contains("templates") && text.contains("validated"),
            "{text}"
        );
        assert!(!text.contains("accesz"), "{text}");
        let wire = serde_json::to_value(&error)?;
        assert!(
            wire["detail"]
                .as_str()
                .ok_or("wire detail")?
                .contains("templates")
        );
        std::fs::remove_file(path)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
        let error = factory
            .graph_document(
                json!({"op":"run", "use":"task.typo", "explain":true}),
                budget,
                &cancel,
            )
            .unwrap_err();
        assert!(
            error
                .diagnostic()
                .detail
                .ok_or("template refusal")?
                .contains("graph input could not be processed")
        );
        let error = factory
            .graph_document(
                json!({"op":"run", "document":{"graph_id":"cycle", "nodes":[
            {"id":"a", "use":"task", "after":["b"]},
            {"id":"b", "use":"task", "after":["a"]}
        ]}, "explain":true}),
                budget,
                &cancel,
            )
            .unwrap_err();
        // A compiled cycle is an admission failure, so it reads the same safe
        // plan refusal as the raw document it compiles to.
        assert!(
            error
                .diagnostic()
                .detail
                .ok_or("patch refusal")?
                .contains("node a: dependency cycle")
        );
        let error = factory
            .graph_document(
                json!({"op":"run", "use":"implement", "brief":"Fix it", "explain":true}),
                budget,
                &cancel,
            )
            .unwrap_err();
        let diagnostic = error.diagnostic();
        assert!(diagnostic.detail.as_deref().is_some_and(|detail| {
            detail.starts_with("node ") && detail.contains("writer worktree")
        }));
        assert!(diagnostic.next.as_deref().is_some_and(|next| {
            next.contains("--worktree PATH") && next.contains("--branch B --base REF")
        }));
        let error = factory
            .graph_document(
                json!({"op":"run", "brief":"Fix it", "access":"worktree_write", "explain":true}),
                budget,
                &cancel,
            )
            .unwrap_err();
        assert!(
            error
                .diagnostic()
                .next
                .as_deref()
                .is_some_and(|next| next.contains("--worktree PATH"))
        );
        Ok(())
    }

    #[test]
    fn writer_controls_preserve_new_branch_and_refuse_conflicting_access() -> Result {
        let cwd = Path::new("/fixture/project");
        let mut node = json!({"id":"task","use":"task"});
        bind_launch_controls(
            &json!({"branch":"fix/parser","base":"origin/main"}),
            &mut node,
            cwd,
        )?;
        assert_eq!(node["worktree"], ".worktrees/fix-parser");
        assert_eq!(
            node["attempt"]["new_worktree"],
            json!({"branch":"fix/parser","base":"origin/main"})
        );
        assert_eq!(node["access"], "worktree_write");
        for input in [
            json!({"worktree":".","access":"read_only"}),
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
    fn writer_template_with_target_defaults_to_writer_access() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        for target in [
            json!({"worktree":".worktrees/fix"}),
            json!({"branch":"fix","base":"origin/main"}),
        ] {
            let mut input = json!({"op":"run","use":"implement","brief":"Fix it"});
            input
                .as_object_mut()
                .unwrap()
                .extend(target.as_object().unwrap().clone());
            let request = factory.child_request(&input)?;
            assert!(request.writer);
            let node = &request
                .launch
                .as_ref()
                .ok_or("missing compiled launch")?
                .node;
            assert_eq!(node["access"], "worktree_write");
        }

        let mut explicit_read = json!({"op":"run","use":"implement","brief":"Fix it","access":"read_only","worktree":".worktrees/fix"});
        assert!(factory.child_request(&explicit_read).is_err());
        explicit_read["access"] = json!("worktree_write");
        let explicit_write = factory.child_request(&explicit_read)?;
        assert!(explicit_write.writer);
        let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
        let cancel = CancelToken::new();
        let explicit_read = factory.graph_document(
            json!({"op":"run","use":"implement","brief":"Inspect","access":"read_only","explain":true}),
            budget,
            &cancel,
        )?;
        assert_eq!(explicit_read["document"]["nodes"][0]["access"], "read_only");
        Ok(())
    }

    #[test]
    fn eight_lane_composite_with_a_file_brief_fits_the_checkpoint_limit() -> Result {
        use provenant_fabric_runtime::graph::{self, AttemptOwner, GraphExecution, GraphJournal};
        #[derive(Default)]
        struct Snapshot(Option<graph::GraphCheckpoint>);
        impl GraphJournal for Snapshot {
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
        struct Waiting;
        impl AttemptOwner for Waiting {
            type Error = std::convert::Infallible;
            fn actor(&self) -> String {
                "checkpoint-fixture".into()
            }
            fn observe_or_start(
                &mut self,
                key: &graph::AttemptKey,
                _: graph::NodeInput<'_>,
                _: &graph::GraphRoute,
                _: graph::AccessMode,
                _: Option<&str>,
                _: graph::Replay<'_>,
            ) -> std::result::Result<graph::AttemptObservation, Self::Error> {
                Ok(graph::AttemptObservation::Pending {
                    attempt_ref: Some(format!("fixture/{}", key.node_id)),
                })
            }
        }
        let rig = TestRig::new()?;
        let mut factory = factory(&rig)?;
        // A large file brief used by eight leaves used to be repeated in every
        // checkpoint projection. Exercise the bundled replacement for that
        // workload, including submission metadata and sealed input manifests.
        let path = rig.root.join("acceptance.txt");
        std::fs::write(&path, "Acceptance evidence.\n".repeat(38_000))?;
        let brief = std::fs::read_to_string(&path)?;
        let limits = &mut factory.context.config.graph.limits;
        limits.node_prompt_bytes = 1_048_576;
        limits.graph_prompt_bytes = 8_388_608;
        limits.concurrency = Some(8);
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 60)?;
        let response = factory.graph_document(
            json!({
                "op":"run", "use":"task-fan-out", "brief":brief,
                "set":{"items":(0..8).map(|n|format!("slice-{n}")).collect::<Vec<_>>()},
                "explain":true,
            }),
            budget,
            &cancel,
        )?;
        let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
        let execution = GraphExecution {
            extra: Default::default(),
            directory: factory.cwd.clone(),
            document: document.clone(),
            review_base: None,
            integration_base: None,
        };
        // The recovery tests cover publication at each transition. Here the
        // property is the size and round trip of the fully populated snapshot;
        // publish it once instead of fsyncing the same large brief per start.
        let first = graph::advance_with_budget(
            &document,
            &mut Waiting,
            &mut Snapshot::default(),
            &factory.context.config.graph.limits,
            &factory.context.config.graph.budget,
        )?;
        {
            let mut journal = FileJournal::open(
                &factory.context.roots.state,
                factory.context.identity.project,
                &document.graph_id,
                budget,
                &cancel,
                &factory.clock,
            )?;
            journal.bind(execution.clone())?;
            journal.save(&first.checkpoint)?;
        }
        assert_eq!(first.status, GraphStatus::Pending);
        assert_eq!(
            first
                .checkpoint
                .nodes
                .iter()
                .filter(|node| !node.attempts.is_empty())
                .count(),
            8
        );
        let inline_bytes =
            serde_json::to_vec(&json!({"execution":execution,"checkpoint":first.checkpoint}))?
                .len();
        let checkpoint_path = rig
            .state
            .join("fabric/graphs")
            .join(factory.context.identity.project.to_string())
            .join(format!("graph-{}.json", document.graph_id));
        let stored = std::fs::read(checkpoint_path)?;
        let limit = 16_777_216;
        assert!(
            inline_bytes > limit,
            "fixture must exceed the former inline limit: {inline_bytes}"
        );
        assert!(stored.len() < limit, "checkpoint is {} bytes", stored.len());
        let envelope: Value = serde_json::from_slice(&stored)?;
        assert!(
            !envelope["stored_text"]
                .as_object()
                .ok_or("prompt references")?
                .is_empty()
        );
        // Short node titles remain readable; the full file brief belongs only
        // in the prompt store, including its copies in submission metadata.
        assert!(!String::from_utf8(stored)?.contains(&brief));
        assert_eq!(
            FileJournal::read_checkpoint(
                &factory.context.roots.state,
                factory.context.identity.project,
                &document.graph_id,
            )?,
            Some(first.checkpoint)
        );
        assert_eq!(
            FileJournal::execution(
                &factory.context.roots.state,
                factory.context.identity.project,
                &document.graph_id,
            )?,
            Some(execution)
        );
        Ok(())
    }

    #[test]
    fn submission_snapshots_repo_templates_and_task_over_has_distinct_briefs() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let directory = rig.project.join(".provenant/templates");
        std::fs::create_dir_all(&directory)?;
        std::fs::write(
            directory.join("review.local.toml"),
            "brief = \"Review local invariants\"\n",
        )?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
        let response = factory.graph_document(
            json!({"op":"run", "use":"task", "over":["alpha","beta"], "explain":true}),
            budget,
            &cancel,
        )?;
        let document: GraphDocument = serde_json::from_value(response["document"].clone())?;
        assert_eq!(
            document.extra["template_library"]["review.local"]["brief"],
            "Review local invariants"
        );
        assert!(
            document.extra["template_resolvers"]
                .get("review-panel")
                .is_some()
        );
        let expanded = document.expanded(&factory.context.config.graph.limits)?;
        let prompts: Vec<_> = expanded
            .nodes
            .iter()
            .filter(|node| node.composite.is_none())
            .map(|node| node.prompt.as_str())
            .collect();
        assert_eq!(prompts.len(), 2);
        assert!(prompts[0].contains("alpha"));
        assert!(prompts[1].contains("beta"));
        assert_ne!(prompts[0], prompts[1]);
        Ok(())
    }

    #[test]
    fn detached_run_is_a_lane_child_control() -> Result {
        let rig = TestRig::new()?;
        let mut factory = factory(&rig)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let input = json!({"op":"run","brief":"Inspect","detached":true,"explain":true});
        assert!(
            factory
                .graph_document(input.clone(), budget, &cancel)
                .is_err()
        );
        factory.context.in_lane = true;
        let preview = factory.graph_document(input, budget, &cancel)?;
        assert_eq!(preview["detached"], true);
        Ok(())
    }

    #[test]
    fn child_launch_preserves_an_unregistered_selected_project_directory() -> Result {
        let rig = TestRig::new()?;
        let mut factory = factory(&rig)?;
        let scratch = rig.root.join("scratch-repository");
        std::fs::create_dir_all(&scratch)?;
        let output = provenant_os::fixture::git_command()
            .arg("init")
            .arg("-q")
            .arg(&scratch)
            .output()?;
        assert!(output.status.success());
        factory.cwd = scratch.clone();
        factory.context =
            Context::open_inspection(factory.context.roots.clone(), &rig.home, &scratch)?;
        let request = factory
            .child_request(&json!({"op":"run","brief":"Read the selected scratch repository"}))?;
        assert_eq!(
            request.launch.as_ref().ok_or("child launch")?.node["attempt"]["directory"],
            json!(scratch)
        );
        assert!(!request.writer);
        Ok(())
    }

    #[test]
    fn child_launch_retains_writer_access_and_has_a_stable_logical_identity() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let input = json!({"op":"run", "use":"implement", "brief":"Fix the parser", "idempotency_key":"parser-launch"});
        let first = factory.child_request(&input)?;
        let unkeyed = json!({"op":"run", "use":"implement", "brief":"Fix the parser"});
        assert_ne!(
            factory.child_request(&unkeyed)?.id,
            factory.child_request(&unkeyed)?.id
        );
        let operation = json!({"op":"run","document":{"use":"task","brief":"Nested operation","operation_id":"child-replay"}});
        assert_eq!(
            factory.child_request(&operation)?,
            factory.child_request(&operation)?
        );
        assert_eq!(
            factory.child_request(&operation)?.idempotency_key,
            "child-replay"
        );
        assert!(
            factory
                .child_request(&json!({"op":"run","operation_id":"bad id"}))
                .is_err()
        );
        assert!(first.writer);
        assert_eq!(
            first.launch.as_ref().unwrap().node["access"],
            "worktree_write"
        );
        let mut retry = input.clone();
        retry["explain"] = true.into();
        retry["wait_seconds"] = 30.into();
        assert_eq!(factory.child_request(&retry)?, first);
        assert_ne!(
            factory
                .child_request(
                    &json!({"op":"run", "use":"implement", "brief":"Fix another parser"})
                )?
                .id,
            first.id
        );
        let bound = factory.child_request(&json!({"op":"run", "document":{"use":"implement", "brief":"Bound writer", "worktree":".worktrees/existing"}}))?;
        let controlled = factory.child_request(&json!({"op":"run", "use":"implement",
            "brief":"Bound controls", "branch":"fix/child", "base":"HEAD",
            "write_scope":["src"], "detached":true}))?;
        let node = &controlled.launch.as_ref().unwrap().node;
        assert_eq!(node["worktree"], ".worktrees/fix-child");
        assert_eq!(
            node["attempt"]["new_worktree"],
            json!({"branch":"fix/child", "base":"HEAD"})
        );
        assert_eq!(controlled.write_scope, ["src"]);
        assert!(controlled.detached);
        let nested = json!({"op":"run", "document":{"use":"implement", "brief":"Nested replay",
            "idempotency_key":"nested-child", "detached":true}});
        let nested_first = factory.child_request(&nested)?;
        assert_eq!(factory.child_request(&nested)?, nested_first);
        assert!(nested_first.detached);
        let mut top = nested.clone();
        top["idempotency_key"] = json!("top-child");
        top["detached"] = json!(false);
        let top = factory.child_request(&top)?;
        assert_eq!(top.idempotency_key, "top-child");
        assert_ne!(top.id, nested_first.id);
        assert!(!top.detached);
        assert!(bound.writer);
        assert!(!bound.needs_worktree_provisioning());
        assert_eq!(
            bound.launch.as_ref().unwrap().node["worktree"],
            ".worktrees/existing"
        );
        Ok(())
    }

    #[test]
    fn authored_documents_validate_schema_version_before_compilation() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
        for document in [
            json!({"schema_version":3, "use":"task"}),
            json!({"schema_version":3, "graph_id":"invalid-version", "nodes":[]}),
            json!({"schema_version":3, "graph_id":"invalid-version", "templates":{}, "nodes":[{"id":"task", "use":"task"}]}),
        ] {
            let input = json!({"op":"run", "document":document, "explain":true});
            let error = factory.child_request(&input).unwrap_err();
            assert!(error.to_string().contains("schema_version"), "{error}");
            let error = factory.graph_document(input, budget, &cancel).unwrap_err();
            assert!(error.to_string().contains("schema_version"), "{error}");
        }
        Ok(())
    }

    #[test]
    fn child_document_refuses_unapplied_controls_by_field_name() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        for field in [
            "network",
            "capability",
            "no_capability",
            "integration_branch",
            "limits",
            "delivery",
            "review_base",
            "decision_session",
            "retry",
            "exclude",
        ] {
            for mut document in [
                json!({"use":"task", "brief":"Inspect"}),
                json!({"graph_id":"child", "schema_version":2, "nodes":[]}),
            ] {
                document[field] = json!(null);
                let error = factory
                    .child_request(&json!({"op":"run", "document":document}))
                    .unwrap_err();
                assert!(error.to_string().contains(field), "{field}: {error}");
            }
        }
        Ok(())
    }

    #[test]
    fn child_launch_compiles_over_set_and_inline_documents() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let request = factory.child_request(&json!({
            "op":"run", "document":{
                "parameters":{"subject":{"type":"string", "default":"default"}},
                "nodes":[{"id":"one", "use":"task", "brief":"Inspect ${subject} ${item}"}]
            }, "set":{"subject":"code"}, "over":["a","b"], "tier":"junior"
        }))?;
        assert!(!request.writer);
        let node = &request.launch.as_ref().unwrap().node;
        fn prompts(node: &Value, found: &mut Vec<String>) {
            if let Some(nodes) = node.pointer("/composite/nodes").and_then(Value::as_array) {
                for child in nodes {
                    prompts(child, found);
                }
            } else if let Some(prompt) = node["prompt"].as_str() {
                found.push(prompt.into());
                assert_eq!(node["route"]["alias"], "junior");
            }
        }
        let mut found = Vec::new();
        prompts(node, &mut found);
        assert_eq!(found, ["Inspect code a", "Inspect code b"]);
        assert!(
            request
                .launch
                .as_ref()
                .unwrap()
                .templates
                .contains_key("lane-document")
        );
        let request = factory.child_request(&json!({
            "op":"run", "document":{"graph_id":"child-doc", "nodes":[
                {"id":"inspect", "use":"task", "brief":"Document child"}
            ]}
        }))?;
        assert_eq!(
            request.launch.as_ref().unwrap().node["composite"]["nodes"][0]["prompt"],
            "Document child"
        );
        let request = factory.child_request(
            &json!({"op":"run", "use":"implement", "brief":"Fix ${item}", "over":["a","b"]}),
        )?;
        assert!(!request.writer);
        let children = request.launch.as_ref().unwrap().node["composite"]["nodes"]
            .as_array()
            .unwrap();
        assert_eq!(children.len(), 2);
        assert!(
            children
                .iter()
                .all(|node| node["access"] == "worktree_write")
        );
        Ok(())
    }

    fn exercise_child_launch(mut input: Value, count: usize, disjoint: bool) -> Result {
        use provenant_fabric_runtime::graph::{GraphCheckpoint, GraphExecution, relay::RelayOp};
        let rig = TestRig::new()?;
        crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "source"])?;
        let mut source = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
        let branch = crate::worktree_tests::git(&rig, &["symbolic-ref", "--short", "HEAD"])?;
        let factory = factory(&rig)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let foreign_checkout = input["worktree"] == "$foreign";
        let explicit_checkout = foreign_checkout || input["worktree"] == "$registered";
        if foreign_checkout {
            let selected = rig.root.join("foreign-project");
            std::fs::create_dir_all(&selected)?;
            for args in [
                vec!["init", "-q"],
                vec!["commit", "--allow-empty", "-qm", "foreign source"],
            ] {
                let result = provenant_os::fixture::git_command()
                    .current_dir(&selected)
                    .args(args)
                    .output()?;
                assert!(
                    result.status.success(),
                    "{}",
                    String::from_utf8_lossy(&result.stderr)
                );
            }
            let result = provenant_os::fixture::git_command()
                .current_dir(&selected)
                .args(["rev-parse", "HEAD"])
                .output()?;
            assert!(result.status.success());
            source = String::from_utf8(result.stdout)?.trim().to_owned();
            let selected_context =
                Context::open(factory.context.roots.clone(), &rig.home, &selected)?;
            provenant_cli::fabric::graph_relay::provision_child(
                &selected_context,
                ".worktrees/explicit-child",
                &source,
                WaitBudget::cli(factory.clock.mono_now(), 30)?,
                &cancel,
            )?;
            input["worktree"] = json!(selected.join(".worktrees/explicit-child"));
        } else if explicit_checkout {
            provenant_cli::fabric::graph_relay::provision_child(
                &factory.context,
                ".worktrees/explicit-child",
                &source,
                WaitBudget::cli(factory.clock.mono_now(), 30)?,
                &cancel,
            )?;
            input["worktree"] = json!(rig.project.join(".worktrees/explicit-child"));
        }
        let document: GraphDocument = serde_json::from_value(json!({
            "schema_version":2, "graph_id":"g-child-e2e", "template_library":{}, "integration_branch":if explicit_checkout { None } else { Some(&branch) },
            "nodes":[{"id":"parent", "access":"read_only", "prompt":"Go", "route":{"alias":"senior"}}]
        }))?;
        let reference = format!("{}/caller", factory.context.identity.host);
        let checkpoint: GraphCheckpoint = serde_json::from_value(json!({
            "schema_version":1, "graph_id":"g-child-e2e", "plan_digest":"fixture", "bound_integration_base":if explicit_checkout { None } else { Some(&source) },
            "nodes":[{"id":"parent", "state":"running", "attempts":[{
                "launched":true, "key":{"graph_id":"g-child-e2e", "node_id":"parent", "route_index":0},
                "route":{"alias":"senior"}, "state":"running", "attempt_ref":reference, "outcome":null, "launch_actor":"fixture"
            }]}]
        }))?;
        {
            let mut journal = FileJournal::open(
                &factory.context.roots.state,
                factory.context.identity.project,
                &document.graph_id,
                budget,
                &cancel,
                &factory.clock,
            )?;
            journal.bind(GraphExecution {
                review_base: None,
                extra: Default::default(),
                integration_base: Some(source.parse()?),
                directory: rig.project.clone(),
                document: document.clone(),
            })?;
            journal.save(&checkpoint)?;
        }
        crate::worktree_tests::git(&rig, &["commit", "--allow-empty", "-m", "host head moved"])?;
        let context = Context::open(factory.context.roots.clone(), &rig.home, &rig.project)?;
        // Retain TMPDIR's short alias: the canonical attempt path can
        // exceed Unix SUN_LEN even though the owned scratch is writable.
        let temporary = std::env::temp_dir();
        let relative = rig.root.strip_prefix(std::fs::canonicalize(&temporary)?)?;
        let socket = temporary.join(relative).join("r/s");
        let _relay = provenant_fabric_mcp::lane::HostRelay::start_with_graph(
            socket.clone(),
            Default::default(),
            |_, _, _| Err(ServiceError::FabricOwnerUnavailable),
            move |arguments, cancel: &CancelToken, _| {
                let clock = SystemClock;
                let budget = WaitBudget::cli(clock.mono_now(), 30)
                    .map_err(|_| ServiceError::FabricOwnerUnavailable)?;
                provenant_cli::fabric::graph_relay::serve(
                    &context, "caller", arguments, budget, cancel, &clock,
                )
            },
        )?;
        {
            let request = factory.child_request(&input)?;
            let frame = serde_json::to_value(RelayOp::Add(request))?;
            let receipt = provenant_fabric_mcp::lane::graph(
                &socket,
                frame.clone(),
                &cancel,
                std::time::Duration::from_secs(30),
            )?;
            let replay = provenant_fabric_mcp::lane::graph(
                &socket,
                frame,
                &cancel,
                std::time::Duration::from_secs(30),
            )?;
            assert_eq!(replay["replayed"], true);
            assert_eq!(replay["node"], receipt["node"]);
            let checkpoint = FileJournal::read_checkpoint(
                &factory.context.roots.state,
                factory.context.identity.project,
                &document.graph_id,
            )?
            .ok_or("missing checkpoint")?;
            assert_eq!(checkpoint.expansions.len(), 1);
            let prefix = receipt["node"].as_str().ok_or("missing child")?;
            let writers: Vec<_> = checkpoint.expansions[0]
                .nodes
                .iter()
                .filter(|node| {
                    node.access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
                        && (node.id == prefix || node.id.starts_with(&format!("{prefix}--")))
                })
                .collect();
            assert_eq!(writers.len(), count);
            let mut paths = std::collections::BTreeSet::new();
            for writer in writers {
                let path = writer.worktree.as_ref().ok_or("missing writer worktree")?;
                let unique = paths.insert(path.clone());
                if disjoint {
                    assert!(unique);
                }
                assert_eq!(
                    crate::worktree_tests::git(&rig, &["-C", path, "rev-parse", "HEAD"])?,
                    source
                );
                let mut owner = BoundGraphOwner {
                    writer_launch_budget: None,
                    admission_timeout: None,
                    launch_warnings: Vec::new(),
                    repo_facts: Default::default(),
                    factory: &factory,
                    integration_base: None,
                    integration_branch: None,
                    network: None,
                    capability_overrides: Default::default(),
                    exclude: Vec::new(),
                    budget,
                    cancel: &cancel,
                    prices: None,
                    tick: false,
                };
                let bound = provenant_fabric_runtime::graph::AttemptOwner::bind_writer_worktree(
                    &mut owner, path,
                )?
                .ok_or("writer binding")?;
                assert_eq!(bound.head, source);
                if foreign_checkout {
                    let state = factory.context.roots.state.as_path();
                    let request =
                        provenant_run::resolve_writer(state, Path::new(path), Path::new(path))?;
                    let selected: provenant_core::ProjectId = serde_json::from_value(
                        request
                            .extra
                            .get("custody_project")
                            .cloned()
                            .ok_or("custody project")?,
                    )?;
                    assert_ne!(selected, factory.context.identity.project);
                    let name = provenant_confinement::worktrees::state_dir_name(Path::new(path));
                    assert!(
                        state
                            .join("worktrees")
                            .join(selected.to_string())
                            .join(&name)
                            .is_dir()
                    );
                    assert!(
                        !state
                            .join("worktrees")
                            .join(factory.context.identity.project.to_string())
                            .join(&name)
                            .exists()
                    );
                    assert!(
                        state
                            .join("writer-projects")
                            .join(factory.context.identity.project.to_string())
                            .join(selected.to_string())
                            .is_dir()
                    );
                    let mut forged = request;
                    forged.extra.insert(
                        "custody_project".into(),
                        json!(factory.context.identity.project),
                    );
                    assert!(
                        provenant_run::prepare_writer(
                            &forged,
                            state,
                            factory.context.identity.project
                        )
                        .is_err()
                    );
                }

                assert_eq!(
                    writer.delivery,
                    (!explicit_checkout).then(|| {
                        provenant_records::run::DeliveryMode::Integrate(branch.clone())
                    })
                );
                assert_eq!(
                    writer.source,
                    (!explicit_checkout).then(|| {
                        provenant_fabric_runtime::graph::SourceBinding::Commit {
                            commit: source.clone(),
                        }
                    })
                );
            }
        }
        Ok(())
    }

    #[test]
    fn explicit_writer_children_use_their_selected_project_custody() -> Result {
        for worktree in ["$foreign", "$registered"] {
            exercise_child_launch(
                json!({"op":"run", "brief":"Write from a read-only parent", "access":"worktree_write", "worktree":worktree}),
                1,
                false,
            )?;
        }
        Ok(())
    }

    #[test]
    fn child_launch_crosses_the_lane_transport_and_provisions_writer_fanout() -> Result {
        for (input, count, disjoint) in [
            (
                json!({"op":"run", "use":"implement", "brief":"Fix the parser"}),
                1,
                false,
            ),
            (
                json!({"op":"run", "use":"implement", "brief":"Fix ${item}", "over":["one","two"]}),
                2,
                false,
            ),
            (
                json!({"op":"run", "document":{
                "parameters":{"subject":{"type":"string"}},
                "nodes":[{"id":"write", "use":"implement", "brief":"Fix ${subject}"}]
            }, "set":{"subject":"inline"}}),
                1,
                false,
            ),
            (
                json!({"op":"run", "document":{"use":"implement", "brief":"Fix ${item}", "write_scope":["${item}"]},
                "over":["three","four"]}),
                2,
                true,
            ),
        ] {
            exercise_child_launch(input, count, disjoint)?;
        }
        Ok(())
    }

    #[test]
    fn child_launch_retains_agent_roots_after_template_parameters_are_bound() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let escaping = json!({"op":"run", "document":{
            "templates":{"escaped":{
                "brief":"Inspect", "tier":"senior", "access":"read_only",
                "parameters":{"root":{"type":"string"}},
                "node":{"read_roots":["${root}"]}
            }}, "use":"escaped"
        }, "set":{"root":rig.home}});
        let request = factory.child_request(&escaping)?;
        let launch = request.launch.ok_or("missing child launch")?;
        assert_eq!(launch.node["read_roots"], json!([rig.home]));
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
        let attempt = json!({"key":{"graph_id":id,"node_id":"task","route_index":0},"launched":true,"route":{"alias":"senior"},"state":"terminal","attempt_ref":"host/attempt","launch_actor":"actor","usage":{"tokens":100,"cost_micro_usd":25,"wall_time_seconds":3,"cache_read_tokens":60,"input_tokens":80,"queue_time_ms":1500,"outcome":"timed_out","tier":"principal","template":"implement.fix"}});
        let mut checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint =
            serde_json::from_value(
                json!({"schema_version":1,"graph_id":id,"plan_digest":"test","nodes":[
                    {"id":"group","worktree":null,"state":"succeeded","attempts":[],"children":["task"],"usage":{"tokens":100}},
                    {"id":"task","worktree":null,"state":"succeeded","attempts":[attempt.clone()],"previous_attempts":[attempt]}
                ]}),
            )?;
        checkpoint.nodes[1].previous_attempts.push(serde_json::from_value(json!({
            "key":{"graph_id":id,"node_id":"task","route_index":1},
            "launched":false,"route":{"alias":"senior"},"state":"terminal",
            "attempt_ref":null,"outcome":"adapter_unavailable","launch_actor":"actor","settled":1
        }))?);
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
        reserved.nodes[1].attempts[0].state =
            provenant_fabric_runtime::graph::AttemptState::Starting;
        reserved.nodes[1].attempts[0].outcome = None;
        reserved.nodes[1].attempts[0].usage = None;
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
        assert_eq!(stats["groups"][0]["name"], "principal");
        assert_eq!(stats["groups"][0]["cache_read_share"], 0.75);
        assert_eq!(stats["groups"][0]["queue_time_ms"], 1500);
        assert_eq!(stats["groups"][0]["outcomes"]["timed_out"], 1);
        let refused = stats["groups"]
            .as_array()
            .ok_or("stats groups")?
            .iter()
            .find(|group| group["name"] == "senior")
            .ok_or("refused attempt group")?;
        assert_eq!(refused["attempts"], 1);
        assert_eq!(refused["outcomes"]["adapter_unavailable"], 1);

        let nodes = factory.graph_document(
            json!({"op":"status","id":id,"stats":true,"by":"node"}),
            budget,
            &cancel,
        )?;
        assert_eq!(nodes["groups"][0]["name"], format!("{id}/task"));
        assert_eq!(nodes["groups"][0]["tokens"], 100);

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
        let refused = retained["groups"]
            .as_array()
            .ok_or("retained groups")?
            .iter()
            .find(|group| group["name"] == "senior")
            .ok_or("retained refusal")?;
        assert_eq!(refused["attempts"], 1);
        assert_eq!(refused["outcomes"]["adapter_unavailable"], 1);

        let all = factory.graph_document(
            json!({"op":"status","stats":true,"by":"template"}),
            budget,
            &cancel,
        )?;
        assert_eq!(all["groups"][0]["attempts"], 1);
        assert_eq!(all["groups"][0]["name"], "implement.fix");
        assert_eq!(all["groups"][0]["cache_read_share"], 0.75);
        assert_eq!(all["groups"][0]["queue_time_ms"], 1500);
        assert_eq!(all["groups"][0]["outcomes"]["timed_out"], 1);
        Ok(())
    }
}
