//! Document-based graph service shared by the CLI and the lane relay.
use super::source_policy::{read_source_report, writer_base_report};
use super::subject::{
    ReadPlan, SubjectAdmission, bind_provisioning_base, bind_read_source,
    resolve_reused_worktree_base,
};
use super::*;
use provenant_fabric_runtime::graph::{
    AccessMode, FileJournal, GraphDocument, GraphJournal, GraphNode, GraphStatus, Layer,
    QueueKnobs, SourceBinding, Template, TemplateLibrary,
    patch::{self, LiveGraph, Patch, PatchContext},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(super) mod observation;
use observation::wait_graph_updates;
pub(super) use observation::wait_submission;

/// A `wait_seconds` that is not a usable wait budget.
fn wait_seconds_invalid() -> FacadeError {
    FacadeError::invalid(InvalidInputDetail::Field("wait_seconds".into()))
}

pub(super) fn graph_input_reason(reason: impl std::fmt::Display) -> FacadeError {
    FacadeError::invalid(InvalidInputDetail::graph_reason(reason))
}

pub(super) fn seal_delivery_queue_knobs(
    config: &provenant_fabric_runtime::config::QueueConfig,
    origins: &BTreeMap<String, provenant_config::Origin>,
    authored: Option<&QueueKnobs>,
) -> Result<QueueKnobs, FacadeError> {
    use provenant_fabric_runtime::graph::template::seal_settings;
    use provenant_fabric_runtime::graph::{
        QUEUE_CI_SLOTS_KEY, QUEUE_MAX_PARALLEL_KEY, QUEUE_PREP_RETRIES_KEY,
    };
    use provenant_records::{Layer, Sourced};

    let keys = [
        QUEUE_MAX_PARALLEL_KEY,
        QUEUE_CI_SLOTS_KEY,
        QUEUE_PREP_RETRIES_KEY,
    ];
    let mut candidates = BTreeMap::<&str, Vec<Sourced<usize>>>::new();
    for (key, value) in [
        (QUEUE_MAX_PARALLEL_KEY, config.max_parallel),
        (QUEUE_CI_SLOTS_KEY, config.ci_slots),
        (QUEUE_PREP_RETRIES_KEY, config.prep_retries),
    ] {
        let source = provenant_cli::config_reads::origin_source(origins.get(key), key);
        if matches!(source.layer, Layer::Host | Layer::Project) {
            candidates
                .entry(key)
                .or_default()
                .push(Sourced { value, source });
        }
    }
    if let Some(authored) = authored {
        for (key, value) in [
            (QUEUE_MAX_PARALLEL_KEY, authored.max_parallel.clone()),
            (QUEUE_CI_SLOTS_KEY, authored.ci_slots.clone()),
            (QUEUE_PREP_RETRIES_KEY, authored.prep_retries.clone()),
        ] {
            candidates.entry(key).or_default().push(value);
        }
    }
    let shipped = QueueKnobs::default();
    let defaults: BTreeMap<&str, Sourced<usize>> = BTreeMap::from([
        (QUEUE_MAX_PARALLEL_KEY, shipped.max_parallel),
        (QUEUE_CI_SLOTS_KEY, shipped.ci_slots),
        (QUEUE_PREP_RETRIES_KEY, shipped.prep_retries),
    ]);
    let sealed = seal_settings(&keys, &defaults, &candidates);
    let knobs = QueueKnobs {
        max_parallel: sealed[QUEUE_MAX_PARALLEL_KEY].clone(),
        ci_slots: sealed[QUEUE_CI_SLOTS_KEY].clone(),
        prep_retries: sealed[QUEUE_PREP_RETRIES_KEY].clone(),
    };
    knobs.validate().map_err(graph_input_reason)?;
    Ok(knobs)
}

fn seal_document_queue(
    document: &mut GraphDocument,
    config: Option<(
        &provenant_fabric_runtime::config::QueueConfig,
        &BTreeMap<String, provenant_config::Origin>,
    )>,
    config_error: Option<&str>,
) -> Result<(), FacadeError> {
    let Some(authored) = document.queue.clone() else {
        return Ok(());
    };
    let sealed = match config {
        Some((queue_config, origins)) => {
            seal_delivery_queue_knobs(queue_config, origins, Some(&authored))?
        }
        None => {
            warn_operator(format!(
                "delivery queue configuration unavailable; using compiled defaults: {}",
                config_error.unwrap_or("unknown configuration error")
            ));
            seal_delivery_queue_knobs(&Default::default(), &BTreeMap::new(), Some(&authored))?
        }
    };
    document.queue = Some(sealed);
    document.schema_version = document.schema_version.max(2);
    Ok(())
}

// Preserve the parser's field path and message for actionable diagnostics.
fn decode_input<T: serde::de::DeserializeOwned>(
    value: Value,
    prefix: &str,
) -> Result<T, FacadeError> {
    serde_path_to_error::deserialize(value).map_err(|error| {
        let path = error.path().to_string();
        let separator = if prefix.is_empty() || path.starts_with('[') || path == "." {
            ""
        } else {
            "."
        };
        graph_input_reason(format!("{prefix}{separator}{path}: {}", error.inner()))
    })
}

fn retry_patch_error(error: patch::PatchError) -> FacadeError {
    use provenant_fabric_runtime::coordination::facade::GraphRelayCause;
    if error.parameter.is_some()
        || error.admission.is_some()
        || error.candidate.is_some()
        || error.code != "invalid"
    {
        return graph_patch_error(error);
    }
    let cause = if error.message.starts_with("unknown node ") {
        GraphRelayCause::RetryUnknownNode
    } else if error.message.contains("settled") {
        GraphRelayCause::RetryUnsettled
    } else if error.message.contains("container retry") {
        GraphRelayCause::RetryContainerBrief
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
pub(super) fn graph_patch_error(error: patch::PatchError) -> FacadeError {
    use provenant_fabric_runtime::coordination::facade::GraphRelayCause;
    if let Some(candidate) = error.candidate.clone() {
        return FacadeError::graph_candidate(*candidate);
    }
    if let Some(admission) = error.admission.clone() {
        return FacadeError::graph_admission(*admission);
    }
    if let Some(refusal) = error.parameter.clone() {
        return FacadeError::invalid(InvalidInputDetail::Parameter(refusal));
    }
    let cause = match error.code {
        "cycle" => Some(GraphRelayCause::Cycle),
        "conflict" | "stale_revision" | "idempotency_conflict" => Some(GraphRelayCause::Conflict),
        "started" => Some(GraphRelayCause::StartedNode),
        "edit_scope" => Some(GraphRelayCause::TargetOutsideGraph),
        "journal" => Some(GraphRelayCause::JournalUnavailable),
        _ => None,
    };
    if let Some(cause) = cause {
        return FacadeError::invalid(InvalidInputDetail::GraphRelayCause(cause));
    }
    graph_input_reason(error)
}

pub(super) fn relay_error(error: ServiceError) -> FacadeError {
    match error {
        ServiceError::Facade(error) => error,
        ServiceError::Mutation { source, .. } => relay_error(*source),
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

fn control_receipt_unfinished(
    run: &provenant_fabric_runtime::graph::GraphRun,
    target: &super::unified::GraphTarget,
    stopping: bool,
) -> bool {
    if stopping {
        run.checkpoint
            .nodes
            .iter()
            .filter(|node| {
                target
                    .node
                    .as_ref()
                    .is_none_or(|id| &node.id == id || node.subtree_cancel.as_deref() == Some(id))
            })
            .flat_map(|node| &node.attempts)
            .any(|attempt| attempt.state != provenant_fabric_runtime::graph::AttemptState::Terminal)
    } else {
        run.status == GraphStatus::Pending
    }
}

/// The launch flags decoded once, here at the host interface edge. Every
/// setting they select is written at `Layer::Launch`, so warnings, receipts
/// and custody can all name the flag that chose it.
#[derive(Clone, Debug, Default)]
struct LaunchFlags {
    checks_mode: Option<String>,
    isolated: bool,
    destination: Option<String>,
    worktree_sharing: Option<String>,
}

impl LaunchFlags {
    fn from_input(input: &Value) -> Self {
        Self {
            checks_mode: input
                .get("checks_mode")
                .and_then(Value::as_str)
                .filter(|value| provenant_records::policy::ChecksMode::from_checks(value).is_some())
                .map(str::to_owned),
            isolated: input["isolated"] == true,
            destination: input
                .get("destination")
                .and_then(Value::as_str)
                .map(str::to_owned),
            worktree_sharing: input
                .get("worktree_sharing")
                .and_then(Value::as_str)
                .map(str::to_owned),
        }
    }
}

/// One launch setting in the receipt, in the same `Sourced` shape other
/// settings use: the value with the layer and origin that selected it.
fn launch_setting(value: Value, origin: &str) -> Value {
    json!({"value": value, "source": {"layer": "launch", "origin": origin}})
}

/// The effective launch-layer settings, reported by both `run --explain` and
/// the launch receipt so the flags are never silently dropped.
fn launch_settings_report(
    flags: &LaunchFlags,
    read_source: &provenant_cli::run::ReadSourceChoice,
) -> Value {
    let mut settings = serde_json::Map::new();
    if let Some(mode) = &flags.checks_mode
        && let Some(strength) = match provenant_records::policy::ChecksMode::from_checks(mode) {
            Some(provenant_records::policy::ChecksMode::Strict) => Some("refuse"),
            Some(provenant_records::policy::ChecksMode::Warn) => Some("warn"),
            Some(provenant_records::policy::ChecksMode::Off) => Some("off"),
            Some(provenant_records::policy::ChecksMode::On) | None => None,
        }
    {
        settings.insert(
            "policy.default".into(),
            launch_setting(json!(strength), "--checks/--strict"),
        );
    }
    if flags.isolated {
        for (key, value, origin) in [
            ("lanes.access", "isolated", "--isolated"),
            ("lanes.sandbox", "on", "--isolated"),
            ("lanes.destination.write", "new", "--isolated"),
            ("lanes.worktree.sharing", "refuse", "--isolated"),
        ] {
            settings.insert(key.into(), launch_setting(json!(value), origin));
        }
        settings.insert(
            "lanes.read_source".into(),
            launch_setting(json!(read_source.source.to_string()), "--isolated"),
        );
    }
    if let Some(destination) = &flags.destination {
        settings.insert(
            "lanes.destination.write".into(),
            launch_setting(json!(destination), "--destination"),
        );
    }
    if let Some(sharing) = &flags.worktree_sharing {
        settings.insert(
            "lanes.worktree.sharing".into(),
            launch_setting(json!(sharing), "--worktree-sharing"),
        );
    }
    Value::Object(settings)
}

/// Attempt settings are reserved launch controls, not template parameters.
pub(super) fn launch_input(input: &Value) -> Result<Value, FacadeError> {
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
            attempt[&key[8..]] = type_attempt_field(&key[8..], value)?;
        }
    }
    for field in ["host", "account"] {
        if let Some(value) = input.get(field).filter(|value| !value.is_null()) {
            attempt[field] = value.clone();
        }
    }
    // `--preamble-file` is a launch-layer input. Carry it on the attempt so
    // `lane_knobs` records it as `lanes.brief_preamble` from the launch layer.
    if let Some(value) = input.get("preamble_file").filter(|value| !value.is_null()) {
        attempt["preamble_file"] = value.clone();
    }
    // The launch flags are launch-layer settings. Carry them on the attempt so
    // `lane_knobs` seals them with Layer::Launch for every consumer: policy,
    // destination, sharing and read source. Explicit flags applied by the CLI
    // after `--isolated` win, and a per-attempt setting wins over the bundle.
    let flags = LaunchFlags::from_input(&input);
    if flags.isolated {
        if attempt.get("lane_access").is_none() {
            attempt["lane_access"] = json!("isolated");
        }
        if attempt.get("sandbox").is_none()
            && let Some(sandbox) = input.get("sandbox").filter(|value| !value.is_null())
        {
            attempt["sandbox"] = sandbox.clone();
        }
    }
    if let Some(mode) = &flags.checks_mode {
        attempt["checks_mode"] = json!(mode);
    }
    if let Some(destination) = &flags.destination {
        attempt["destination"] = json!(destination);
    }
    if let Some(sharing) = &flags.worktree_sharing {
        attempt["worktree_sharing"] = json!(sharing);
    }
    // Retain provenance when these run controls are merged onto a template.
    for field in ["token_budget", "on_budget", "budget_grace", "lane_access"] {
        if attempt.get(field).is_some() {
            if !attempt["budget_sources"].is_object() {
                attempt["budget_sources"] = json!({});
            }
            attempt["budget_sources"][field] = json!("launch");
        }
    }
    let typed: provenant_fabric_runtime::graph::AttemptSpec =
        decode_input(attempt.clone(), "attempt")?;
    if typed.names_cloud_placement() {
        return Err(FacadeError::invalid(
            InvalidInputDetail::CloudPlacementRemoved,
        ));
    }
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
    Ok(input)
}

/// A `--set attempt.FIELD=VALUE` that arrived as text takes the first reading
/// the attempt schema accepts: TOML (`true`, `300`, `["a"]`), then the text as
/// written. A value that arrived typed, as it does from the lane relay, is left alone.
fn type_attempt_field(field: &str, value: Value) -> Result<Value, FacadeError> {
    let Value::String(text) = &value else {
        return Ok(value);
    };
    let parsed = toml_edit::de::from_str::<Value>(&format!("value = {text}"))
        .ok()
        .and_then(|mut table| table.get_mut("value").map(Value::take));
    for candidate in parsed.into_iter().chain([value.clone()]) {
        if decode_input::<provenant_fabric_runtime::graph::AttemptSpec>(
            json!({ field: candidate }),
            "attempt",
        )
        .is_ok()
        {
            return Ok(candidate);
        }
    }
    Err(FacadeError::invalid(InvalidInputDetail::FieldDecode {
        field: format!("attempt.{field}"),
        missing: false,
    }))
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
                | provenant_fabric_runtime::graph::NodeKind::Hygiene
                | provenant_fabric_runtime::graph::NodeKind::Publish
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
    base: Option<&str>,
    head: &str,
) {
    // An explicit composite source is inherited by its members. Loops resolve
    // their own subjects as writers advance, rather than freezing every round.
    if node.source.is_some() || node.loop_body.is_some() {
        return;
    }
    if let Some(composite) = &mut node.composite {
        for child in &mut composite.nodes {
            bind_submission_subject(child, base, head);
        }
    } else if node.review_behaviour.is_some() {
        // The subject is materialised as an attempt-private detached view that
        // becomes the lane's working directory (#3714). Say so explicitly: a
        // host-access lane can still reach the primary checkout, and a model
        // told only about a SHA will check it out there itself (#3943).
        // The base is authoritative too: the detached view carries no remote
        // default ref and the lane cannot fetch, so a reviewer must not try to
        // confirm the merge base against one (#3977).
        let mut paragraph = format!(
            "\n\nWhen a review range is supplied, the host pins its commits and supplies Git context. The subject is already checked out at {head} in your working directory, a dedicated read-only detached view; read it there. The host-supplied head at {head} is authoritative"
        );
        if let Some(base) = base {
            paragraph.push_str(&format!(
                ", as is the host-supplied base at {base}; the base is available as the named ref {} in your working directory. Do not fetch or try to confirm either commit against a remote default ref, which this view does not carry",
                provenant_run::REVIEW_BASE_REF
            ));
        } else {
            paragraph.push_str(
                "; do not fetch or check out another commit to confirm it, which this view cannot do",
            );
        }
        paragraph.push_str(
            ". Do not check out, switch or reset any other checkout, including the project's primary checkout.",
        );
        node.prompt.push_str(&paragraph);
        node.source = Some(provenant_fabric_runtime::graph::SourceBinding::Commit {
            commit: head.into(),
        });
    }
}

fn bind_review_base(node: &mut provenant_fabric_runtime::graph::GraphNode, base: &str) {
    node.review_base = Some(base.into());
    if let Some(composite) = &mut node.composite {
        for child in &mut composite.nodes {
            bind_review_base(child, base);
        }
    }
}

/// Whether a node, or one of its members, carries a review protocol. A
/// composite boundary owns no protocol of its own; its lens members do.
fn carries_review_protocol(node: &GraphNode) -> bool {
    node.review_behaviour.is_some()
        || node.delivery_review.is_some()
        || node.role.as_deref().is_some_and(|role| {
            role == "lens" || role.starts_with("lens.") || role.starts_with("review.")
        })
        || node.template.as_deref().is_some_and(|name| {
            name == "findings-panel" || name == "review-panel" || name.starts_with("review")
        })
        || node
            .composite
            .as_ref()
            .is_some_and(|composite| composite.nodes.iter().any(carries_review_protocol))
        || node
            .loop_body
            .as_ref()
            .is_some_and(|loop_spec| loop_spec.body.iter().any(carries_review_protocol))
}

/// Whether a node reads one named writer by node source and carries a review
/// protocol. Only those nodes compare base..head; other readers of a writer's
/// head (hygiene, publication, conflict) have no review range to resolve.
fn is_node_sourced_review_of(node: &GraphNode, writer: &str) -> bool {
    matches!(&node.source, Some(SourceBinding::Node { node }) if node == writer)
        && node.access == AccessMode::ReadOnly
        && carries_review_protocol(node)
}

fn has_node_sourced_review(node: &GraphNode, writer: &str) -> bool {
    is_node_sourced_review_of(node, writer)
        || node.composite.as_ref().is_some_and(|composite| {
            composite
                .nodes
                .iter()
                .any(|child| has_node_sourced_review(child, writer))
        })
        || node.loop_body.as_ref().is_some_and(|loop_spec| {
            loop_spec
                .body
                .iter()
                .any(|child| has_node_sourced_review(child, writer))
        })
}

/// Whether this writer has a node-sourced review without a panel-specific or
/// graph-authored comparison base.
fn has_unbased_node_sourced_review(node: &GraphNode, writer: &str) -> bool {
    (is_node_sourced_review_of(node, writer) && node.review_base.is_none())
        || node.composite.as_ref().is_some_and(|composite| {
            composite
                .nodes
                .iter()
                .any(|child| has_unbased_node_sourced_review(child, writer))
        })
        || node.loop_body.as_ref().is_some_and(|loop_spec| {
            loop_spec
                .body
                .iter()
                .any(|child| has_unbased_node_sourced_review(child, writer))
        })
}

/// Any review node that sources `writer` gets the resolved base. The composite
/// boundary and its members are both pinned so every lens receives base..head.
fn bind_writer_review_base(node: &mut GraphNode, writer: &str, base: &str) {
    if is_node_sourced_review_of(node, writer) {
        bind_review_base(node, base);
    }
    if let Some(composite) = &mut node.composite {
        for child in &mut composite.nodes {
            bind_writer_review_base(child, writer, base);
        }
    }
    if let Some(loop_spec) = &mut node.loop_body {
        for child in &mut loop_spec.body {
            bind_writer_review_base(child, writer, base);
        }
    }
}

/// Warn when a user-authored review brief asks for a verdict line outside the
/// standalone PASS/FAIL/INCONCLUSIVE contract. ResultCapture remains the owner
/// of parsing and can normalise common wording, but launch should expose drift
/// before a reviewer spends time on the task.
fn review_verdict_contract_warning(input: &Value) -> Option<String> {
    fn brief_strings<'a>(value: &'a Value, output: &mut Vec<&'a str>) {
        match value {
            Value::Object(fields) => {
                for (key, value) in fields {
                    if matches!(key.as_str(), "brief" | "prompt")
                        && let Some(text) = value.as_str()
                    {
                        output.push(text);
                    }
                    brief_strings(value, output);
                }
            }
            Value::Array(values) => values.iter().for_each(|value| brief_strings(value, output)),
            _ => {}
        }
    }

    fn mismatched_declaration(line: &str) -> bool {
        let lower = line.to_ascii_lowercase();
        let mut rest = lower.as_str();
        while let Some(marker) = rest.find("verdict") {
            if marker > 0
                && rest[..marker]
                    .chars()
                    .next_back()
                    .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            {
                rest = &rest[marker + "verdict".len()..];
                continue;
            }
            rest = &rest[marker + "verdict".len()..];
            let after_word = rest.trim_start_matches(char::is_whitespace);
            let has_contract_colon = after_word.starts_with(':');
            let trimmed = after_word
                .strip_prefix(':')
                .unwrap_or(after_word)
                .trim_start_matches(|ch: char| {
                    ch.is_whitespace() || matches!(ch, '`' | '*' | '_' | '[' | '(')
                });
            let token_end = trimmed
                .find(|ch: char| {
                    ch.is_whitespace()
                        || matches!(ch, ',' | ';' | '.' | ':' | ']' | ')' | '`' | '*' | '_')
                })
                .unwrap_or(trimmed.len());
            let token = trimmed[..token_end].trim_end_matches(':');
            let canonical = matches!(token, "pass" | "fail" | "inconclusive");
            let trailing = trimmed[token_end..].trim_matches(|ch: char| {
                ch.is_whitespace() || matches!(ch, ',' | ';' | '.' | ']' | ')' | '`' | '*' | '_')
            });
            let option_list = trailing.starts_with("verdict")
                || trailing.starts_with("or verdict")
                || trailing.starts_with("and verdict");
            if !has_contract_colon || !canonical || (!trailing.is_empty() && !option_list) {
                return true;
            }
            rest = &trimmed[token_end..];
        }
        false
    }

    let mut briefs = Vec::new();
    brief_strings(input, &mut briefs);
    briefs
        .into_iter()
        .flat_map(str::lines)
        .find(|line| mismatched_declaration(line))
        .map(|line| {
            let line = line.trim();
            let excerpt = line.chars().take(160).collect::<String>();
            let excerpt = if line.chars().count() > 160 {
                format!("{excerpt}…")
            } else {
                excerpt
            };
            format!(
                "review_verdict_contract_mismatch: brief asks for `{excerpt}`; the contract requires one standalone VERDICT: PASS, FAIL or INCONCLUSIVE line"
            )
        })
}

fn bind_review_warning(node: &mut provenant_fabric_runtime::graph::GraphNode, warning: &str) {
    if let Some(composite) = &mut node.composite {
        for child in &mut composite.nodes {
            bind_review_warning(child, warning);
        }
    } else if node.review_behaviour.is_some() {
        node.prompt.insert_str(0, &format!("{warning}\n\n"));
    }
}

/// Launch controls that bind a writer; a composite template can take none of them.
const LAUNCH_CONTROLS: [&str; 5] = ["access", "worktree", "branch", "base", "write_scope"];

/// The resolved node a template or `use` launch runs, taken before the graph
/// id is chosen. Its lowered roles decide whether the launch is a writer.
struct TemplateLaunch {
    node: Value,
    integration_branch: Option<String>,
}

impl TemplateLaunch {
    /// Whether the lowered launch runs a worktree writer under a condition
    /// that holds. Read-source defaults belong to readers; a writer starts
    /// from its own base however the launch is expressed.
    fn writes_worktree(
        &self,
        library: &TemplateLibrary,
        resolvers: &BTreeMap<String, Vec<String>>,
    ) -> Result<bool, FacadeError> {
        let lowered = provenant_fabric_runtime::graph::template::Compiler { library, resolvers }
            .lower(&self.node)
            .map_err(compile_error)?;
        Ok(lowered_writes_worktree(&lowered))
    }
}

/// Whether a lowered node, its members or its loop body write a worktree.
/// A condition that does not hold removes the node and everything under it.
fn lowered_writes_worktree(node: &Value) -> bool {
    let holds = node
        .get("when")
        .and_then(|terms| {
            serde_json::from_value::<Vec<provenant_fabric_runtime::graph::WhenTerm>>(terms.clone())
                .ok()
        })
        .is_none_or(|terms| provenant_fabric_runtime::graph::when::holds(&terms));
    if !holds {
        return false;
    }
    if node.get("access").and_then(Value::as_str) == Some("worktree_write") {
        return true;
    }
    ["/composite/nodes", "/loop/body"]
        .into_iter()
        .any(|pointer| {
            node.pointer(pointer)
                .and_then(Value::as_array)
                .is_some_and(|children| children.iter().any(lowered_writes_worktree))
        })
}

/// Read the branch a linked checkout has checked out from its Git admin dir.
/// This is a read-only observation: a missing or unreadable registration is
/// not evidence of the requested branch, so the ordinary provisioning path
/// still reports the precise cause.
fn linked_worktree_branch(path: &Path) -> Option<String> {
    let marker = path.join(".git");
    let text = std::fs::read_to_string(marker).ok()?;
    let gitdir = text.trim().strip_prefix("gitdir:")?.trim();
    let directory = PathBuf::from(gitdir);
    let directory = if directory.is_absolute() {
        directory
    } else {
        path.join(directory)
    };
    let head = std::fs::read_to_string(directory.join("HEAD")).ok()?;
    head.trim()
        .strip_prefix("ref: refs/heads/")
        .map(str::to_owned)
}

/// Bind one writer target for `--pr`, `--branch` and `--worktree`. A branch
/// whose managed linked checkout already exists is bound in place like
/// `--worktree`; only a branch with no such checkout is provisioned from its
/// base. Occupancy and dirty-worktree admission stay downstream, so a held
/// checkout still refuses with its holder and remedy.
fn bind_writer_target(
    node: &mut Value,
    path: &Path,
    provision: Option<(&str, &str, bool)>,
) -> Result<(), FacadeError> {
    node["worktree"] = json!(path);
    let Some((branch, base, default_base)) = provision else {
        return Ok(());
    };
    if linked_worktree_branch(path).as_deref() == Some(branch) {
        // A document may have carried a provisioning intent for this branch;
        // the existing checkout supersedes it.
        if let Some(attempt) = node.get_mut("attempt").and_then(Value::as_object_mut) {
            attempt.remove("new_worktree");
        }
        warn_operator(format!(
            "branch {branch} already has linked worktree {}; launching into that checkout with --worktree semantics",
            path.display()
        ));
        return Ok(());
    }
    if node.get("attempt").is_none() {
        node["attempt"] = json!({});
    }
    if !node["attempt"].is_object() {
        return Err(FacadeError::invalid(InvalidInputDetail::Field(
            "attempt".into(),
        )));
    }
    node["attempt"]["new_worktree"] = json!({"branch":branch,"base":base});
    if default_base {
        node["attempt"]["new_worktree"]["default_base"] = json!(true);
    }
    Ok(())
}

/// Apply plain-task execution controls before template lowering and patch admission.
fn bind_launch_controls(
    input: &Value,
    node: &mut Value,
    project_root: &Path,
    ports: &provenant_config::PortTable,
) -> Result<(), FacadeError> {
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
            // The real default is resolved at admission from `lanes.writer_base`;
            // `default_base` tells provisioning to resolve it again on the peer.
            None => "HEAD",
        };
        provenant_confinement::repo::GitRevision::new(base).map_err(|_| invalid("base"))?;
        let worktree = ports
            .managed_writer_path(branch)
            .map_err(|_| invalid("branch"))?;
        bind_writer_target(
            node,
            &project_root.join(worktree),
            Some((branch, base, input.get("base").is_none_or(Value::is_null))),
        )?;
    } else if let Some(worktree) = worktree {
        let path = Path::new(
            worktree
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| invalid("worktree"))?,
        );
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            project_root.join(path)
        };
        bind_writer_target(node, &path, None)?;
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
            json!({"kind":"attempt", "attempt_id":"host/one", "settlement_class":"pre_launch_refusal", "preparation_failure":"auth_required"}),
            json!({"kind":"attempt", "attempt_id":"host/two", "settlement_class":"pre_launch_refusal", "preparation_failure":"usage_limited"}),
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

    /// Spec: docs/specs/run-settlement.md#review-verdict-wording
    #[test]
    fn review_launch_warns_when_brief_declares_a_different_verdict_shape() {
        for brief in [
            "VERDICT: approve <sha>",
            "VERDICT: PASS 0be4e6c",
            "Verdict FAIL: reason",
            "verdict: maybe",
        ] {
            let warning = super::review_verdict_contract_warning(&json!({"brief":brief}))
                .unwrap_or_else(|| panic!("expected a warning for {brief:?}"));
            assert!(warning.contains("review_verdict_contract_mismatch"));
            assert!(warning.contains(brief));
        }
        for brief in [
            "VERDICT: PASS",
            "verdict: FAIL",
            "VERDICT: INCONCLUSIVE",
            "Write VERDICT: PASS, VERDICT: FAIL or VERDICT: INCONCLUSIVE",
        ] {
            assert_eq!(
                super::review_verdict_contract_warning(&json!({"brief":brief})),
                None,
                "{brief:?}"
            );
        }
    }
}

// Project durable attempt facts after the owner has tried fallback routes.
fn report_launch_refusals(result: &mut Value, rows: &[Value]) {
    let refusals: Vec<_> = rows
        .iter()
        .filter(|row| row["kind"] == "attempt" && row["settlement_class"] == "pre_launch_refusal")
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

/// The one PR repair fact resolved from the forge and CI observer before an
/// implement launch. The branch and base are launch controls; `failures` is a
/// bounded section appended to the brief.
struct PrRepairFact {
    branch: String,
    base: String,
    failures: String,
}

impl Factory {
    /// Resolve `run implement --pr N` from the one observer: the PR's head
    /// branch and base become launch controls, and the failing checks (names
    /// and bounded log tails) are appended to the brief. Review templates keep
    /// their own subject path. Spec: docs/specs/cli.md#ci-observations
    fn apply_pr_repair(&self, input: Value) -> Result<Value, FacadeError> {
        if input.get("pr").is_none_or(|value| value.is_null()) {
            return Ok(input);
        }
        self.apply_pr_repair_with(input, |number| self.observe_pr_repair(number))
    }

    /// The injectable resolution seam an invariant pins without a live forge.
    fn apply_pr_repair_with(
        &self,
        mut input: Value,
        resolve: impl FnOnce(u64) -> Result<PrRepairFact, FacadeError>,
    ) -> Result<Value, FacadeError> {
        let is_implement = input
            .get("template")
            .and_then(Value::as_str)
            .is_some_and(|name| name == "implement" || name.starts_with("implement."));
        let number = input
            .get("pr")
            .filter(|value| !value.is_null())
            .and_then(Value::as_u64);
        if !is_implement || number.is_none_or(|number| number == 0) {
            return Ok(input);
        }
        let number =
            number.ok_or_else(|| FacadeError::invalid(InvalidInputDetail::Field("pr".into())))?;
        let fact = resolve(number)?;
        if input.get("branch").is_none_or(Value::is_null) {
            input["branch"] = json!(fact.branch);
        }
        if input.get("base").is_none_or(Value::is_null) {
            input["base"] = json!(fact.base);
        }
        if !fact.failures.is_empty() {
            let brief = input.get("brief").and_then(Value::as_str).unwrap_or("");
            let mut augmented = brief.to_owned();
            augmented.push_str("\n\n");
            augmented.push_str(&fact.failures);
            input["brief"] = json!(augmented);
        }
        // The PR has been consumed by the repair; leave no advisory pin for a
        // review subject to reinterpret.
        if let Some(object) = input.as_object_mut() {
            object.remove("pr");
            object.remove("head");
        }
        Ok(input)
    }

    /// Read the pull and its failing checks through the one CI observer, never
    /// a caller-owned `gh` process. Spec: docs/specs/cli.md#ci-observations
    fn observe_pr_repair(&self, number: u64) -> Result<PrRepairFact, FacadeError> {
        use std::time::Duration;
        let invalid = |reason: String| FacadeError::invalid(InvalidInputDetail::Field(reason));
        let request = provenant_config::ports::Request::within(Duration::from_secs(60));
        let state = self.context.roots.state.as_path();
        let project = &self.context.project_root;
        let repository = crate::pr::observer::repository_for_project(state, project, &request)
            .map_err(|error| invalid(error.to_string()))?;
        let mut observer =
            crate::pr::observer::Observer::open(state, project, &repository, &request)
                .map_err(|error| invalid(error.to_string()))?;
        let resource = observer
            .resource(&format!("pulls/{number}"), Duration::ZERO, &request)
            .map_err(|error| invalid(error.to_string()))?;
        let field = |pointer: &str| -> Result<&str, FacadeError> {
            resource
                .pointer(pointer)
                .and_then(Value::as_str)
                .ok_or_else(|| invalid(format!("pull resource has no {pointer}")))
        };
        let head_sha = field("/head/sha")?.to_owned();
        let branch = field("/head/ref")?.to_owned();
        let base = field("/base/ref")?.to_owned();
        let checks = observer
            .checks(&head_sha, Duration::ZERO, &request)
            .map_err(|error| invalid(error.to_string()))?;
        let mut failures = format!("CI failures for pull request #{number} at {head_sha}:\n");
        let mut failed = 0;
        for check in checks.iter().filter(|check| check.bucket == "fail") {
            failed += 1;
            let tail: String = check.excerpt.chars().take(2048).collect();
            failures.push_str(&format!("- {}: {}\n", check.name, tail));
        }
        if failed == 0 {
            failures.clear();
        }
        Ok(PrRepairFact {
            branch,
            base,
            failures,
        })
    }

    #[allow(
        clippy::print_stderr,
        reason = "warn operator when policy is unavailable and defaults are used"
    )]
    pub(super) fn graph_input_warnings(&self, value: &Value) -> Result<Vec<String>, FacadeError> {
        let mut policy =
            match provenant_cli::config_reads::read_for(&self.context.roots, "run admission") {
                Ok(config) => config.domains.policy.resolve_with_knobs(
                    &self.context.identity.project.to_string(),
                    &config.knobs,
                    &Default::default(),
                ),
                Err(error) => {
                    let error = provenant_records::diagnostic::safe_text(&error.to_string());
                    provenant_records::warning::warn(
                    provenant_records::warning::Warning::new(
                        provenant_records::warning::CONFIG_UNAVAILABLE,
                        format!(
                            "full config unavailable ({error}); using the owner's resolved policy"
                        ),
                        provenant_records::warning::detected_source(
                            provenant_records::warning::CONFIG_UNAVAILABLE,
                        ),
                    )
                    .subject(self.context.identity.project.to_string()),
                );
                    provenant_config::policy::read(
                        self.context.roots.state.as_path(),
                        &self.context.identity.project.to_string(),
                        &Default::default(),
                    )
                }
            };
        let flags = LaunchFlags::from_input(value);
        if let Some(mode) = flags
            .checks_mode
            .as_deref()
            .and_then(provenant_records::policy::ChecksMode::from_checks)
        {
            policy.apply_checks(mode);
        }
        Ok(
            provenant_fabric_runtime::graph::refusal::input_warnings(value, &policy)
                .into_iter()
                .map(|warning| warning.message)
                .collect(),
        )
    }

    /// Resolve a node's declared host to this host or a configured peer. The
    /// placement check and the read-only explanation share this resolution, so
    /// an unknown name is refused in both and an alias is never mistaken for a
    /// peer (or the reverse).
    fn node_host(&self, node: &GraphNode) -> Result<Option<provenant_core::HostId>, FacadeError> {
        let Some(name) = node
            .attempt
            .as_ref()
            .and_then(|attempt| attempt.host.as_deref())
        else {
            return Ok(None);
        };
        let local = self.context.identity.host;
        if matches!(name, "local" | "localhost")
            || name
                .parse::<provenant_core::HostId>()
                .is_ok_and(|host| host == local)
        {
            return Ok(Some(local));
        }
        self.context
            .peer_host(name)
            .map(Some)
            .ok_or(FacadeError::UnknownHost)
    }

    fn validate_host_placement(&self, node: &GraphNode) -> Result<(), FacadeError> {
        if !provenant_fabric_runtime::graph::when::holds(&node.when) {
            return Ok(());
        }
        if let Some(host) = self.node_host(node)? {
            let local = self.context.identity.host;
            if host == local
                && node.access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
                && node.worktree.as_deref().is_none_or(str::is_empty)
            {
                return Err(FacadeError::WorktreeNotApplicable);
            }
            if host != local {
                // A peer never accepts a caller path. It may provision a writer
                // itself from a branch and a base ref, which carry no path.
                let new_worktree = node
                    .attempt
                    .as_ref()
                    .and_then(|attempt| attempt.new_worktree.as_ref());
                let caller_path = node
                    .worktree
                    .as_deref()
                    .is_some_and(|path| !path.is_empty());
                let peer_writer = node.access
                    == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
                    && !caller_path
                    && new_worktree.is_some();
                if node.access != provenant_fabric_runtime::graph::AccessMode::ReadOnly
                    && !peer_writer
                    || caller_path
                    || new_worktree.is_some()
                        && node.access != provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
                {
                    return Err(FacadeError::WorktreeNotApplicable);
                }
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

    /// Whether this submission binds a node to another host. Such a document
    /// crosses a host boundary, so it must not carry this host's replay
    /// snapshot: that snapshot retains file-backed brief paths and other
    /// sender-local attempt state. The peer rebuilds its own snapshot from the
    /// compiled node and the sealed classification (#3744).
    fn document_is_peer_bound(&self, document: &GraphDocument) -> bool {
        fn node_is_peer_bound(
            node: &provenant_fabric_runtime::graph::GraphNode,
            context: &Context,
        ) -> bool {
            let remote = |name: &str| {
                !matches!(name, "local" | "localhost")
                    && context
                        .peer_host(name)
                        .is_some_and(|host| host != context.identity.host)
            };
            node.attempt
                .as_ref()
                .and_then(|attempt| attempt.host.as_deref())
                .is_some_and(remote)
                || node.composite.as_ref().is_some_and(|composite| {
                    composite
                        .nodes
                        .iter()
                        .any(|child| node_is_peer_bound(child, context))
                })
                || node.loop_body.as_ref().is_some_and(|body| {
                    body.body
                        .iter()
                        .any(|child| node_is_peer_bound(child, context))
                })
                || node
                    .plan
                    .as_ref()
                    .and_then(|plan| plan.fallback.as_ref())
                    .is_some_and(|fallback| {
                        fallback
                            .nodes
                            .iter()
                            .any(|child| node_is_peer_bound(child, context))
                    })
        }
        document
            .nodes
            .iter()
            .any(|node| node_is_peer_bound(node, &self.context))
    }

    fn launch_resolvers(
        &self,
        document: Option<&Value>,
    ) -> Result<BTreeMap<String, Vec<String>>, FacadeError> {
        self.launch_resolvers_for_tier(document, None, None)
    }

    fn launch_resolvers_for_tier(
        &self,
        document: Option<&Value>,
        route: Option<&RouteChoice>,
        review_range: Option<(&str, &str)>,
    ) -> Result<BTreeMap<String, Vec<String>>, FacadeError> {
        let config =
            provenant_cli::config_reads::read_for(&self.context.roots, "run").map_err(|error| {
                FacadeError::invalid(InvalidInputDetail::Configuration(error.to_string()))
            })?;
        // An unknown tier is refused before panel resolution, so advisory and
        // strict diversity both give this usage error instead of a compile error.
        for name in route
            .map(RouteChoice::tiers)
            .unwrap_or_default()
            .into_iter()
            .filter(|name| !name.contains('/'))
        {
            let catalogue = provenant_routing::Catalogue::bundled()
                .map_err(|_| FacadeError::InvalidInput { detail: None })?;
            let names = config.domains.routing.route_names(&catalogue);
            if !names.contains(name) {
                let valid = names.into_iter().collect::<Vec<_>>().join(", ");
                return Err(FacadeError::DispatchRefused {
                    fact: None,
                    disk: None,
                    reason: format!("unknown named route `{name}`; valid tiers: {valid}"),
                });
            }
        }
        // Resolver selection needs review context, before template nodes compile.
        let mut resolver_document =
            json!({"schema_version":2,"graph_id":"launch-resolvers","nodes":[]});
        for field in ["review", "context", "exclude"] {
            if let Some(value) = document.and_then(|document| document.get(field)) {
                resolver_document[field] = value.clone();
            }
        }
        if let Some(tier) = route.and_then(|route| route.tier_for("review")) {
            resolver_document["review_tier_override"] = json!(tier);
        }
        let document: GraphDocument = decode_input(resolver_document, "document")?;
        let mut resolvers = provenant_cli::fabric::graph_relay::launch_resolvers_at_roots(
            &self.context.roots,
            &self.cwd,
            &config.domains.routing.review,
            &document,
            review_range,
        )
        .map_err(|reason| FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason,
        })?;
        resolvers.insert(
            provenant_fabric_runtime::graph::template::IMPLEMENT_PLAN_DEFAULT.into(),
            vec![config.domains.fabric.implement.plan.to_string()],
        );
        resolvers.insert(
            provenant_fabric_runtime::graph::template::BATCH_MAX_PARALLEL_DEFAULT.into(),
            vec![config.domains.fabric.batch.max_parallel.to_string()],
        );
        Ok(resolvers)
    }

    pub(crate) fn relay_document(
        &self,
        input: &Value,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Value, FacadeError> {
        use provenant_fabric_runtime::graph::relay::RelayOp;
        let pinned_review = input.get("pr").is_some_and(|value| !value.is_null())
            || input.pointer("/set/subject").is_some()
            || input.pointer("/set/review_base").is_some();
        let request = if pinned_review {
            // The lane has read-only Git metadata and no fetch authority. The
            // host owns PR lookup, ref resolution and missing-object preparation.
            let mut input = input.clone();
            // One unnamed invocation stays replayable if its transport reply is
            // lost; a separate invocation still gets a distinct child.
            if input.get("name").is_none_or(Value::is_null)
                && input
                    .get("idempotency_key")
                    .or_else(|| input.pointer("/document/idempotency_key"))
                    .is_none_or(Value::is_null)
            {
                input["idempotency_key"] = json!(provenant_core::hex(
                    &provenant_os::fs::entropy_128().map_err(|_| FacadeError::StateCorrupt)?
                ));
            }
            let identity =
                provenant_fabric_runtime::graph::relay::RunRequestIdentity::for_input(&input)
                    .map_err(|_| FacadeError::StateCorrupt)?;
            RelayOp::Run {
                request_id: identity.id,
                input,
            }
        } else {
            RelayOp::Add(self.child_request(input)?)
        };
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
            .map_err(|_| wait_seconds_invalid())?;
        let launch = match &request {
            provenant_fabric_runtime::graph::relay::RelayOp::Add(child) if child.has_writers() => {
                let writer = provenant_cli::fabric::graph_relay::writer_launch_budget(
                    &self.context,
                    &self.clock,
                    budget,
                );
                WaitBudget::up_to(
                    self.clock.mono_now(),
                    budget
                        .remaining(self.clock.mono_now())
                        .max(writer.remaining(self.clock.mono_now())),
                )
            }
            _ => budget,
        };
        // Resolve the observation cadence before launching, so a malformed
        // configuration refuses the request rather than losing its receipt.
        let cadence = settlement.map(|_| self.wait_cadence()).transpose()?;
        let within =
            std::time::Duration::from_micros(launch.remaining(self.clock.mono_now()).as_micros())
                .saturating_add(std::time::Duration::from_secs(1));
        let receipt = provenant_cli::fabric::graph_relay::from_lane(&request, cancel, within)
            .map_err(relay_error)?;
        if let (Some(settlement), Some(cadence)) = (settlement, cadence) {
            let remaining = settlement.remaining(self.clock.mono_now()).as_micros();
            if remaining == 0 {
                return Ok(json!({"receipt":receipt,"observation_error":"child wait timed out"}));
            }
            let observation = provenant_cli::fabric::graph_relay::child_wait_observation(&cadence);
            return Ok(
                match provenant_cli::fabric::graph_relay::wait_for_children(
                    cancel,
                    std::time::Duration::from_micros(remaining),
                    cadence.interval,
                    provenant_cli::fabric::graph_relay::ChildWaitMode::All,
                    &mut |_| {},
                ) {
                    Ok(children) => {
                        json!({"receipt":receipt,"children":children,"observation":observation})
                    }
                    Err(error) => {
                        json!({"receipt":receipt,"observation_error":error.to_string(),"observation":observation})
                    }
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
        let input = self.apply_pr_repair(launch_input(input)?)?;
        let input = &input;
        if input.get("template").and_then(Value::as_str) == Some("deliver")
            && input.get("over").is_some_and(|value| !value.is_null())
        {
            return Err(graph_input_reason(
                "deliver --over is only supported by the top-level durable queue dispatcher",
            ));
        }
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
        // A name is the child's node id. Distinct unnamed calls create distinct
        // children; a call replays by an explicit key, or by its name and input.
        let name = match input.get("name").filter(|value| !value.is_null()) {
            Some(value) => {
                let name = value.as_str().ok_or_else(|| {
                    FacadeError::invalid(InvalidInputDetail::Field("name".into()))
                })?;
                let folded = provenant_fabric_runtime::graph::template::fold_graph_name(name)
                    .map_err(graph_input_reason)?;
                Some(folded)
            }
            None => None,
        };
        let idempotency_key = match input
            .get("idempotency_key")
            .or_else(|| raw.and_then(|document| document.get("idempotency_key")))
        {
            Some(value) => value
                .as_str()
                .filter(|key| !key.is_empty() && key.len() <= 200)
                .ok_or_else(invalid)?
                .to_owned(),
            None if name.is_some() => submission_digest(input, None, None)?,
            None => provenant_core::hex(
                &provenant_os::fs::entropy_128().map_err(|_| FacadeError::StateCorrupt)?,
            ),
        };
        let key = match &name {
            Some(name) => name.clone(),
            None => {
                let digest =
                    provenant_records::CanonicalV1::file_digest(idempotency_key.as_bytes())
                        .to_string();
                format!("child-{}", &digest[7..39])
            }
        };
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
        let route = route_choice(input, raw)?;
        let has_overrides = ["brief", "template", "route", "over"]
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
        // Top-level queue selectors are dispatched before this nested-run compiler.
        let library = self.template_library(Some(&inline))?;
        let template_name = input
            .get("template")
            .and_then(Value::as_str)
            .or_else(|| node["use"].as_str())
            .unwrap_or("task");
        let review = library
            .resolve(template_name)
            .map_err(|_| invalid())?
            .accepts_review_range();
        let review_subject = self.review_subject(input, review, &CancelToken::new())?;
        let idempotency_key = if let Some((subject, _)) = &review_subject
            && name.is_some()
            && input
                .get("idempotency_key")
                .or_else(|| raw.and_then(|document| document.get("idempotency_key")))
                .is_none()
        {
            let mut pinned = input.clone();
            pinned["subject"] = json!({"base":subject.base,"head":subject.head});
            submission_digest(&pinned, Some(&subject.head.to_string()), None)?
        } else {
            idempotency_key
        };
        if full_document.is_none() {
            // The CLI brief is a node prompt; template parameters retain their names.
            for (field, node_field) in [("brief", "prompt"), ("over", "over")] {
                if let Some(value) = input.get(field).filter(|value| !value.is_null()) {
                    node[node_field] = value.clone();
                }
            }
            if let Some(name) = input.get("template").filter(|value| !value.is_null()) {
                node["use"] = name.clone();
            }
            if let Some(RouteChoice::Alias(alias)) = &route {
                node["tier"] = json!(alias);
            }
            let template_name = node["use"].as_str().unwrap_or("task").to_owned();
            let template = library.resolve(&template_name).map_err(|_| invalid())?;
            let mut parameters = node.get("with").cloned().unwrap_or_else(|| json!({}));
            if let Some(overrides) = input.get("set").filter(|value| !value.is_null()) {
                let fields = parameters.as_object_mut().ok_or_else(invalid)?;
                for (name, value) in overrides.as_object().ok_or_else(invalid)? {
                    if review && matches!(name.as_str(), "subject" | "review_base") {
                        continue;
                    }
                    fields.insert(
                        name.clone(),
                        typed_binding(&template, &template_name, name, value)?,
                    );
                }
            }
            if template.parameters.contains_key("brief")
                && let Some(brief) = input.get("brief").filter(|value| !value.is_null())
            {
                parameters["brief"] = brief.clone();
            }
            if template.is_composite() {
                let fields = node.as_object_mut().ok_or_else(invalid)?;
                fields.remove("prompt");
                fields.remove("tier");
            }
            node["with"] = parameters;
        }
        bind_launch_controls(
            input,
            &mut node,
            &self.context.project_root,
            &self.context.port_table,
        )?;
        node["id"] = json!(key);
        let range = review_subject.as_ref().and_then(|(subject, _)| {
            subject
                .base
                .map(|base| (base.to_string(), subject.head.to_string()))
        });
        let range = range
            .as_ref()
            .map(|(base, head)| (base.as_str(), head.as_str()));
        let resolvers = self.launch_resolvers_for_tier(raw, route.as_ref(), range)?;
        let mut compiled = provenant_fabric_runtime::graph::template::Compiler {
            library: &library,
            resolvers: &resolvers,
        }
        .lower(&node)
        .map_err(compile_error)?;
        if let Some(choice) = input
            .get("route")
            .filter(|value| !value.is_null())
            .and(route.as_ref())
        {
            let mut typed = serde_json::from_value(compiled).map_err(|_| invalid())?;
            override_tier(&mut typed, choice);
            compiled = encode(typed)?;
        }
        let mut typed = serde_json::from_value(compiled).map_err(|_| invalid())?;
        override_launch(&mut typed, input)?;
        if let Some((subject, warning)) = &review_subject {
            if let Some(base) = subject.base {
                bind_submission_subject(
                    &mut typed,
                    Some(&base.to_string()),
                    &subject.head.to_string(),
                );
                bind_review_base(&mut typed, &base.to_string());
            }
            if let Some(warning) = warning {
                warn_operator(warning);
                bind_review_warning(&mut typed, warning);
            }
        }
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
            prompt: input
                .get("prompt")
                .or_else(|| input.get("brief"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            // The compiled launch already carries an exact model route. Keeping
            // it as a tier here would let relay child controls replace it with
            // an alias, which cannot hold a model id.
            tier: match &route {
                Some(RouteChoice::Alias(alias)) if !alias.contains('/') => Some(alias.clone()),
                _ => None,
            },
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
        let worktrees = match crate::graph_support::project_worktrees(&self.cwd) {
            crate::graph_support::ProjectWorktrees::Registered(paths) => paths,
            crate::graph_support::ProjectWorktrees::NotRepository => vec![self.cwd.clone()],
            crate::graph_support::ProjectWorktrees::Unavailable(reason) => {
                let reason = provenant_records::diagnostic::safe_text(&reason);
                provenant_records::warning::warn(
                    provenant_records::warning::Warning::new(
                        provenant_records::warning::WORKTREE_INVENTORY_UNAVAILABLE,
                        format!("repository worktree inventory unavailable ({reason}); read roots are limited to the current directory"),
                     provenant_records::warning::detected_source(provenant_records::warning::WORKTREE_INVENTORY_UNAVAILABLE))
                    .subject(self.cwd.display().to_string()),
                );
                vec![self.cwd.clone()]
            }
        };
        let policy = provenant_fabric_runtime::read_roots::ReadRoots::new(
            worktrees,
            &self.context.config.graph.allowed_read_roots,
        );
        crate::graph_support::confine_pattern_roots(node, &policy)
            .map_err(|_| FacadeError::invalid(InvalidInputDetail::Field("read_roots".into())))?;
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
                *root = encode(policy.confine(&path).map_err(|_| {
                    FacadeError::invalid(InvalidInputDetail::Field("read_roots".into()))
                })?)?;
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
            .load_dir(Layer::Host, &config.join("provenant/templates"))
            .map_err(|_| FacadeError::invalid(InvalidInputDetail::TemplateInvalid))?;
        library
            .load_dir(
                Layer::Project,
                &self.cwd.join(provenant_config::repository_files::TEMPLATES),
            )
            .map_err(|_| FacadeError::invalid(InvalidInputDetail::TemplateInvalid))?;
        if let Some(inline) = inline.filter(|value| !value.is_null()) {
            let templates: BTreeMap<String, Template> = serde_json::from_value(inline.clone())
                .map_err(|_| FacadeError::invalid(InvalidInputDetail::TemplateInvalid))?;
            library = library
                .with(Layer::Launch, &templates)
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
        self.graph_document_announcing(input, budget, cancel, None)
    }

    /// `graph_document`, announcing a submitted graph's first saved checkpoint.
    pub(crate) fn graph_document_announcing(
        &self,
        input: Value,
        budget: WaitBudget,
        cancel: &CancelToken,
        announce: Option<super::graph_execution::Announce<'_>>,
    ) -> Result<Value, FacadeError> {
        if input.get("queue_control").is_some() {
            if self.context.in_lane || self.context.inspection_only {
                return Err(FacadeError::ConfinementRefused { resource: None });
            }
            return super::queue_controls::handle_graph_operation(self, input, budget, cancel);
        }
        let operation = required(&input, "op")?;
        if operation == "run"
            && input.get("template").and_then(Value::as_str) == Some("deliver")
            && input.get("over").is_some_and(|value| !value.is_null())
        {
            return Err(graph_input_reason(
                "deliver --over must enter through the top-level durable queue dispatcher",
            ));
        }
        let warnings = if matches!(operation, "edit" | "retry") {
            self.graph_input_warnings(&input)?
        } else {
            Vec::new()
        };
        // This shared path must not silently drop the retired field.
        if input.get("operation_id").is_some() {
            return Err(FacadeError::invalid(InvalidInputDetail::OperationIdRetired));
        }
        if let Some(wait) = input.get("wait_seconds") {
            let seconds = wait.as_u64().ok_or_else(wait_seconds_invalid)?;
            WaitBudget::cli(self.clock.mono_now(), seconds).map_err(|_| wait_seconds_invalid())?;
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
                let patch: Patch = decode_input(input["patch"].clone(), "patch")?;
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
        let mut receipt = match operation {
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
                            "template":library.resolve(name).map_err(graph_input_reason)?,
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
                self.submit_document(&input, budget, cancel, announce)
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
        }?;
        if !warnings.is_empty() {
            receipt["warnings"] = json!(warnings);
        }
        Ok(receipt)
    }

    fn submit_document(
        &self,
        input: &Value,
        budget: WaitBudget,
        cancel: &CancelToken,
        announce: Option<super::graph_execution::Announce<'_>>,
    ) -> Result<Value, FacadeError> {
        if super::queue_dispatch::is_queue_submission(input) {
            return super::queue_dispatch::submit_queue(self, input, budget, cancel, announce);
        }
        let submitted = input;
        let prepared = launch_input(input)?;
        let queue_launch = if prepared.get("queue_item").is_some() {
            let registry =
                super::queue_registry::QueueRegistry::open(self.context.roots.state.as_path())
                    .map_err(|_| FacadeError::StateCorrupt)?;
            let project = self.context.identity.project.to_string();
            let execution_directory = self.cwd.to_string_lossy().into_owned();
            Some(
                registry
                    .authorise_launch(
                        &project,
                        &execution_directory,
                        &prepared,
                        budget,
                        cancel,
                        &self.clock,
                    )
                    .map_err(|_| FacadeError::Conflict)?,
            )
        } else {
            None
        };
        let queue_replay_digest = queue_launch
            .as_ref()
            .map(|_| submission_digest(&prepared, None, None))
            .transpose()?;
        if let Some(launch) = &queue_launch
            && let Some(execution) = FileJournal::execution(
                &self.context.roots.state,
                self.context.identity.project,
                &launch.item.graph_id,
            )
            .map_err(graph_journal_error)?
        {
            if !super::queue_scheduler::matches_item_execution(
                &launch.queue.queue_id,
                &launch.item,
                &execution,
            )
            .map_err(|_| FacadeError::StateCorrupt)?
            {
                return Err(FacadeError::Conflict);
            }
            let run = self.graph(
                GraphInput::Status {
                    extra: Default::default(),
                    id: launch.item.graph_id.clone(),
                },
                budget,
                cancel,
            )?;
            let mut result = encode(run)?;
            result["timed_out"] = json!(false);
            result["named"] = json!({"requested":launch.item.graph_id, "graph":launch.item.graph_id, "replayed":true});
            return Ok(result);
        }
        let mut input = self.apply_pr_repair(prepared)?;
        if let Some(launch) = &queue_launch {
            let branch = if let Some(adoption) = &launch.item.adoption {
                adoption
                    .get("branch")
                    .and_then(Value::as_str)
                    .ok_or(FacadeError::StateCorrupt)?
                    .to_owned()
            } else {
                format!("queue/{}", launch.item.graph_id)
            };
            provenant_confinement::repo::GitRefName::branch(&branch)
                .map_err(|_| FacadeError::StateCorrupt)?;
            let worktree = if let Some(adoption) = &launch.item.adoption {
                PathBuf::from(
                    adoption
                        .get("worktree")
                        .and_then(Value::as_str)
                        .filter(|path| Path::new(path).is_absolute())
                        .ok_or(FacadeError::StateCorrupt)?,
                )
            } else {
                self.context.project_root.join(
                    self.context
                        .port_table
                        .managed_writer_path(&branch)
                        .map_err(|_| FacadeError::StateCorrupt)?,
                )
            };
            if input.get("set").is_none_or(Value::is_null) {
                input["set"] = json!({});
            }
            input["set"]
                .as_object_mut()
                .ok_or(FacadeError::StateCorrupt)?
                .insert("worktree".into(), json!(worktree));
        }
        let raw_owned = input.get("document").cloned();
        let raw = raw_owned.as_ref();
        let mut input_warnings = self.graph_input_warnings(&input)?;
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
            "template",
            "route",
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
        let route = route_choice(&input, raw)?;
        let template_name = input["template"]
            .as_str()
            .or_else(|| lane_document.and_then(|document| document["use"].as_str()))
            .unwrap_or(if lane_document.is_some() {
                "lane-document"
            } else {
                "task"
            })
            .to_owned();
        let template_name = template_name.as_str();
        // Type each `--set` value once, by the template's declaration, before
        // the launch is hashed or compiled.
        let declared = library.resolve(template_name).map_err(graph_input_reason)?;
        if let Some(overrides) = input.get("set").and_then(Value::as_object).cloned() {
            let mut typed = serde_json::Map::new();
            for (name, value) in &overrides {
                typed.insert(
                    name.clone(),
                    typed_binding(&declared, template_name, name, value)?,
                );
            }
            input["set"] = Value::Object(typed);
        }
        // `--isolated` pins the read source to this HEAD at the launch layer.
        // Resolve it once here, before admission reads the source choice, so
        // the sealed subject carries the Launch provenance too.
        if LaunchFlags::from_input(&input).isolated
            && input.get("source").is_none_or(Value::is_null)
            && let Some(head) = provenant_os::git::git_executable()
                .and_then(|git| provenant_run::resolve_git_base(&git, &self.cwd, "HEAD").ok())
        {
            input["source"] = json!(head);
        }
        let input = &input;
        let source_choice = input.get("source").filter(|value| !value.is_null());
        if let Some(choice) = source_choice
            && choice
                .as_str()
                .is_none_or(|value| provenant_cli::run::ReadSource::parse(value).is_err())
        {
            return Err(FacadeError::invalid(InvalidInputDetail::Field(
                "source".into(),
            )));
        }
        // One subject per admission: review ranges, read sources and writer
        // bases are resolved together, once, and bound into this graph's
        // identity. Nothing below re-reads a ref or re-resolves a source.
        // The authored document's own review field is not the launch contract;
        // only the selected template's acceptance decides review resolution.
        let review_launch = declared.accepts_review_range();
        let remote = input
            .get("host")
            .and_then(Value::as_str)
            .is_some_and(|host| !host.is_empty() && !matches!(host, "local" | "localhost"));
        // A managed queue item has no checkout yet. Resolve its writer base
        // from the same default a provisioned `queue/<id>` branch would use,
        // so that one commit is pinned into the item branch and worktree
        // intent at the single admission point. Adopted items carry their own
        // base and keep resolving it from the real checkout.
        let queue_base_input = queue_launch
            .as_ref()
            .filter(|launch| launch.item.adoption.is_none())
            .map(|launch| {
                let mut selection = input.clone();
                selection["branch"] = json!(format!("queue/{}", launch.item.graph_id));
                selection
            });
        let SubjectAdmission {
            subject,
            writer_base,
            read,
            warnings: subject_warnings,
            review_warning,
            read_source: mut read_source,
            writer_base_setting,
            read_pin,
            writer_warnings,
        } = self.admit_subject(
            input,
            queue_base_input.as_ref(),
            review_launch,
            remote,
            cancel,
        )?;
        input_warnings.extend(subject_warnings);
        let flags = LaunchFlags::from_input(input);
        if flags.isolated {
            // `--isolated` pins the read source to the admitted HEAD at the
            // launch layer, without re-reading a ref after admission.
            if let Some(head) = subject.as_ref().map(|subject| subject.head.to_string()) {
                read_source.source = provenant_cli::run::ReadSource::Ref(head);
            }
            read_source.layer = Layer::Launch;
            read_source.origin = Some("--isolated".into());
        }
        let review_pin = subject
            .as_ref()
            .filter(|_| review_launch)
            .map(|subject| json!({"base":subject.base,"head":subject.head}));
        let mut resolved_head = subject.as_ref().map(|subject| subject.head.to_string());
        let resolved_review_base = subject
            .as_ref()
            .and_then(|subject| subject.base)
            .map(|base| base.to_string());
        let review_range = resolved_review_base
            .as_deref()
            .zip(resolved_head.as_deref());
        let resolver = self.launch_resolvers_for_tier(raw, route.as_ref(), review_range)?;
        let context = PatchContext {
            catalogue: None,
            library: &library,
            resolvers: &resolver,

            journal: None,
            limits: &self.context.config.graph.limits,
            worktree_layout: self.context.port_table.worktree_layout.clone(),
        };
        // Admission resolved the sourced read-source and writer-base settings
        // once; the queue settings below read the same run configuration.
        let read_config_result = provenant_cli::config_reads::read_for(&self.context.roots, "run");
        let read_config_error = read_config_result.as_ref().err().map(ToString::to_string);
        let read_config = read_config_result.ok();
        // Resolve the graph a template launch runs before sealing its identity.
        // Writer-ness is the resolved roles' answer, not one launch key's.
        let template_launch = if raw.is_none() || lane_document.is_some() {
            Some(self.template_launch(
                input,
                raw,
                lane_document,
                route.as_ref(),
                &context,
                &mut input_warnings,
            )?)
        } else {
            None
        };
        let writer_launch = match &template_launch {
            Some(launch) => launch.writes_worktree(&library, &resolver)?,
            None => false,
        };
        // Admission produced the writer base, the read subject and the pin it
        // came from; writer-ness decides which one this launch keeps. A writer
        // starts from its resolved base, which is then the graph's only read
        // commit, so a moving checkout HEAD never enters a writer's identity. A
        // review keeps its range subject either way.
        let mut writer_base = writer_base.filter(|_| writer_launch);
        if writer_launch {
            input_warnings.extend(writer_warnings);
        }
        let subject = subject.filter(|_| review_launch || writer_base.is_none());
        let default_source =
            read_pin.filter(|_| raw.is_none() && subject.is_some() && !review_launch);
        // The commit the launch starts from belongs to its identity.
        let source_commit = if review_launch {
            resolved_head.clone()
        } else {
            writer_base
                .map(|base| base.to_string())
                .or_else(|| subject.as_ref().map(|subject| subject.head.to_string()))
        };
        if !review_launch {
            resolved_head = source_commit.clone();
        }
        // The name is a handle; the digest of the launch decides whether it is
        // the same launch (replay) or a new one (the next free NAME-k).
        let requested = input.get("name").and_then(Value::as_str).or_else(|| {
            raw.and_then(|document| document.get("graph_id"))
                .and_then(Value::as_str)
        });
        // A queue item owns one fixed graph handle. Its launch digest remains
        // part of the submission identity, so journal custody can accept an
        // exact replay and reject a competing launch under the same handle.
        let strict_queue_id = input
            .get("queue_item")
            .map(|item| {
                let valid = item.is_object()
                    && item["queue_id"]
                        .as_str()
                        .is_some_and(|value| !value.is_empty())
                    && item["launch_digest"]
                        .as_str()
                        .is_some_and(|value| value.starts_with("sha256:"))
                    && ["project", "tracker", "item"].iter().all(|field| {
                        item["identity"][field]
                            .as_str()
                            .is_some_and(|value| !value.is_empty())
                    });
                let graph_id = item["graph_id"].as_str().filter(|value| !value.is_empty());
                if valid && graph_id.is_some() {
                    Ok(graph_id)
                } else {
                    Err(FacadeError::invalid(InvalidInputDetail::Field(
                        "queue_item".into(),
                    )))
                }
            })
            .transpose()?;
        let strict_queue_id = strict_queue_id.flatten();
        if let Some(queue_id) = strict_queue_id
            && requested.is_none_or(|name| name != queue_id)
        {
            return Err(FacadeError::invalid(InvalidInputDetail::Field(
                "queue_item.graph_id".into(),
            )));
        }
        let (base, named) = match requested {
            Some(name) => {
                let folded = provenant_fabric_runtime::graph::template::fold_graph_name(name)
                    .map_err(graph_input_reason)?;
                (folded, true)
            }
            None if template_name == "lane-document" => ("graph".to_owned(), false),
            None => (
                provenant_fabric_runtime::graph::template::default_graph_base(template_name),
                false,
            ),
        };
        let mut templates = effective_templates(&library, template_name, input);
        templates["$implement_plan_default"] = json!(self.context.config.implement.plan);
        templates["$batch_max_parallel_default"] = json!(self.context.config.batch.max_parallel);
        let mut identity_input = input.clone();
        if let Some(subject) = &subject {
            identity_input["subject"] = json!({"base":subject.base,"head":subject.head});
        }
        let digest = match queue_replay_digest {
            Some(digest) => digest,
            None => submission_digest(&identity_input, source_commit.as_deref(), Some(&templates))?,
        };
        let explain = input["explain"] == true;
        let (generated_id, taken) = if explain {
            (
                if named {
                    base.clone()
                } else {
                    format!("{base}-1")
                },
                None,
            )
        } else {
            use provenant_fabric_runtime::graph::template::{NameDecision, decide_graph_name};
            let state = &self.context.roots.state;
            let project = self.context.identity.project;
            let existing =
                FileJournal::reserved_graph_ids(state, project).map_err(graph_journal_error)?;
            let stored = |id: &str| {
                FileJournal::execution(state, project, id)
                    .ok()
                    .flatten()
                    .and_then(|execution| execution.document.replay_digest().map(str::to_owned))
            };
            match decide_graph_name(&base, &digest, named, &existing, stored)
                .map_err(graph_input_reason)?
            {
                NameDecision::Replay(id) => {
                    if strict_queue_id.is_some_and(|queue_id| id != queue_id) {
                        return Err(FacadeError::Conflict);
                    }
                    // The same input is the same launch: report that graph as
                    // it stands and start nothing. Advancing it stays with the
                    // scheduler and explicit recovery.
                    let run = self.graph(
                        GraphInput::Status {
                            extra: Default::default(),
                            id: id.clone(),
                        },
                        budget,
                        cancel,
                    )?;
                    let mut result = encode(run)?;
                    result["timed_out"] = json!(false);
                    result["named"] = json!({"requested":requested, "graph":id, "replayed":true});
                    return Ok(result);
                }
                NameDecision::Create { id, taken } => {
                    if strict_queue_id.is_some_and(|queue_id| id != queue_id) {
                        return Err(FacadeError::Conflict);
                    }
                    (id, taken)
                }
            }
        };
        let id = generated_id.as_str();
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
                patch::compile_submission(id, &patch, &context, raw["integration_branch"].as_str())
                    .map_err(graph_patch_error)?
            } else {
                let mut document = decode_input::<GraphDocument>(raw.clone(), "document")?;
                document.graph_id = id.to_owned();
                document
            }
        } else {
            let launch = template_launch.as_ref().ok_or_else(|| {
                FacadeError::invalid(InvalidInputDetail::Field("document".into()))
            })?;
            patch::compile_submission(
                id,
                &Patch {
                    schema_version: patch::PATCH_SCHEMA_VERSION,
                    nodes: vec![launch.node.clone()],
                    ..Default::default()
                },
                &context,
                launch.integration_branch.as_deref(),
            )
            .map_err(graph_patch_error)?
        };
        // Borrow the optional base: the prompt binding below still needs it
        // after the document has been given its own copy.
        // The host overwrites any caller-supplied subject and review base with
        // the one it admitted. Both are full object IDs from Git.
        if let Some(base) = subject.as_ref().and_then(|subject| subject.base) {
            document.review_base = Some(base.to_string());
            document.schema_version = document.schema_version.max(2);
        }
        document.subject = subject.clone();
        document.template_layers = library.snapshot_layers();
        document.template_library = library.snapshot();
        document.template_resolvers = resolver;
        if let Some(raw) = raw {
            let mut compiled = encode(&document)?;
            for key in [
                "limits",
                "queue",
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
            document = decode_input(compiled, "document")?;
        }
        // #4064: a writer that reuses an existing worktree has no launch-level
        // base. Resolve one from the node (declared base, the branch's upstream,
        // else its merge base with the default branch) so a findings-panel that
        // sources that writer compares base..pushed head instead of an empty
        // range. A plain writer stays advisory: only a node-sourced review
        // needs the base to be present before launch.
        // Admission now resolves its own git internally; this later lookup of a
        // reused worktree's upstream/merge-base still needs the executable, so
        // bind it here rather than relying on a name that no longer exists.
        let git = provenant_os::git::git_executable();
        let mut reused_writer_bases = Vec::new();
        for node in &document.nodes {
            if node.access == AccessMode::WorktreeWrite && node.worktree.is_some() {
                reused_writer_bases.push((
                    node.id.clone(),
                    resolve_reused_worktree_base(
                        node,
                        &self.context.project_root,
                        &self.cwd,
                        self.context.roots.state.as_path(),
                        &read_source,
                        git.as_deref(),
                    ),
                ));
            }
        }
        // The source that chose a reused writer's base, when it became the
        // launch's writer base; the report names it instead of the knob.
        let mut writer_base_source = None;
        for (writer_id, base) in &reused_writer_bases {
            if writer_base.is_none()
                && reused_writer_bases.len() == 1
                && let Some(base) = base.as_ref()
                && let Some(commit) = base
                    .commit
                    .as_deref()
                    .and_then(|commit| provenant_core::GitOid::parse(commit).ok())
            {
                writer_base = Some(commit);
                writer_base_source = Some(base.source.label());
            }
            match base.as_ref().and_then(|base| base.commit.as_deref()) {
                Some(commit) => {
                    let has_review = document
                        .nodes
                        .iter()
                        .any(|node| has_node_sourced_review(node, writer_id));
                    for node in &mut document.nodes {
                        bind_writer_review_base(node, writer_id, commit);
                    }
                    if has_review {
                        document.schema_version = document.schema_version.max(2);
                    }
                }
                None if document.review_base.is_none()
                    && document
                        .nodes
                        .iter()
                        .any(|node| has_unbased_node_sourced_review(node, writer_id)) =>
                {
                    return Err(FacadeError::invalid(InvalidInputDetail::Field(format!(
                        "review_base_unresolved: writer {writer_id} reuses a worktree with no declared base, upstream or default-branch merge base; declare base on the node or supply review_base"
                    ))));
                }
                None => {}
            }
        }
        // NameDecision::Replay returns above. A graph_id in the raw document
        // can still name a new full-document submission, so queue presence is
        // the only selector here.
        let queue_config = read_config
            .as_ref()
            .map(|config| (&config.domains.fabric.queue, &config.origins));
        seal_document_queue(&mut document, queue_config, read_config_error.as_deref())?;
        // The admitted base is a GitOid; bind_item takes a borrowed ref string.
        let writer_base_commit = writer_base.as_ref().map(|base| base.to_string());
        if let Some(launch) = &queue_launch {
            super::queue_delivery::bind_item(
                self,
                launch,
                &mut document,
                writer_base_commit.as_deref(),
            )?;
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
            && input.get("route").is_some_and(|value| !value.is_null())
            && let Some(choice) = &route
        {
            for node in &mut document.nodes {
                override_tier(node, choice);
            }
        }
        for node in &mut document.nodes {
            override_launch(node, input)?;
            self.validate_host_placement(node)?;
        }
        if let Err(reason) =
            provenant_cli::route::validate_graph_exclusions(&document.exclude, &self.context.roots)
        {
            return Err(FacadeError::DispatchRefused {
                reason,
                disk: None,
                fact: None,
            });
        }
        if let Some(head) = &resolved_head {
            for node in &mut document.nodes {
                bind_submission_subject(node, resolved_review_base.as_deref(), head);
            }
            document.schema_version = document.schema_version.max(u64::from(
                provenant_fabric_runtime::graph::minimum_schema_version(&document),
            ));
        }
        // A writer's read-only members take its base; every other pinned read
        // node takes the admitted subject's head. A live checkout keeps its
        // working tree and carries no source binding at all.
        let read_commit = writer_base.or_else(|| {
            subject
                .as_ref()
                .filter(|_| read == ReadPlan::Pinned)
                .map(|subject| subject.head)
        });
        // A local writer provisions its new worktree from the admitted base.
        // A peer resolves its own default, since this commit may not exist in
        // its clone; `default_base` tells it to.
        if !remote && let Some(commit) = writer_base.map(|commit| commit.to_string()) {
            for node in &mut document.nodes {
                bind_provisioning_base(node, &commit);
            }
        }
        if let Some(commit) = read_commit.map(|commit| commit.to_string()) {
            for node in &mut document.nodes {
                bind_read_source(node, &commit);
            }
            document.schema_version = document.schema_version.max(u64::from(
                provenant_fabric_runtime::graph::minimum_schema_version(&document),
            ));
        }
        let mut bound = encode(&document)?;
        self.confine_nodes(&mut bound)?;
        // Template binding can assemble a credential from individually safe
        // inputs. Check the bound launch before submission normalises its text.
        input_warnings.extend(self.graph_input_warnings(&bound)?);
        document = decode_input(bound, "document")?;
        if review_launch && let Some(warning) = review_verdict_contract_warning(input) {
            input_warnings.push(warning.clone());
            for node in &mut document.nodes {
                bind_review_warning(node, &warning);
            }
        }
        if let Some(warning) = &review_warning {
            for node in &mut document.nodes {
                bind_review_warning(node, warning);
            }
        }
        // Fix the document's own spelling here, before it is sealed or replayed,
        // so explain shows exactly the ids admission will read verbatim.
        // Spec: docs/specs/fabric.md#one-model-spelling-per-route
        let catalogue = provenant_cli::route::effective_catalogue(&self.context.roots).ok();
        // The host owns the effective catalogue and decides every model
        // spelling here, once. Admission reads the stored ids verbatim.
        // Spec: docs/specs/fabric.md#one-model-spelling-per-route
        input_warnings.extend(
            provenant_fabric_runtime::graph::canonicalise_route_models(
                &mut document,
                catalogue.as_ref(),
            )
            .into_iter()
            .map(|warning| warning.message),
        );
        document.set_replay_digest(&digest);
        // Explain is read-only and still shows the compiled submission. For a
        // launch, the host performs writer preflight before sealing the patch.
        if input["explain"] == true {
            let submission_key = format!("submit:{}", document.graph_id);
            patch::submission(
                &mut document,
                &submission_key,
                &self.context.config.graph.limits,
                self.context.port_table.worktree_layout.clone(),
            )
            .map_err(graph_patch_error)?;
            // The snapshot is a host-local replay record: it embeds the
            // file-backed brief path and other sender placement state. A
            // peer-bound document ships the brief as prompt text and the
            // classification in its sealed policy, so withhold the snapshot
            // and let the peer rebuild one from its own bound node (#3744).
            if self.document_is_peer_bound(&document) {
                document.submission_patch = None;
                warn_operator(
                    "withheld sender-local submission snapshot from peer graph document; the peer binds its own brief inputs",
                );
            }
        }
        // Static admission is the single compile gate: a launch no longer seals
        // the submission patch above, so admit here to keep every entry on the
        // same typed (redacted) refusal.
        provenant_fabric_runtime::graph::admit_candidate(
            &document,
            None,
            &[],
            &self.context.config.graph.limits,
            &self.context.config.graph.budget,
            Some(&self.requester_lane_path()?),
        )
        .map_err(FacadeError::graph_candidate)?;
        if !writer_launch && let Some(selected) = &default_source {
            let warning =
                provenant_cli::run::read_only_source_warning(&self.cwd, selected, &read_source);
            warn_operator(&warning);
            input_warnings.push(warning);
        }
        input_warnings.sort();
        input_warnings.dedup();
        let warnings = input_warnings;
        if input["explain"] == true {
            let skipped = provenant_fabric_runtime::graph::when::skipped_rows(
                &document,
                &self.context.config.graph.limits,
            );
            let config = provenant_cli::config_reads::read_for(&self.context.roots, "run")
                .map_err(|error| FacadeError::DispatchRefused {
                    fact: None,
                    disk: None,
                    reason: error.to_string(),
                })?;
            let run_config = provenant_confinement::run_config::RunConfig::read(
                self.context.roots.state.as_path(),
            )
            .unwrap_or_default();
            let expanded = document
                .expanded(&self.context.config.graph.limits)
                .map_err(|error| FacadeError::DispatchRefused {
                    fact: None,
                    disk: None,
                    reason: error.to_string(),
                })?;
            // Explain runs the launch's pre-attempt admission checks, so an
            // explanation that passes means the same input admits a launch.
            super::graph_execution::validate_explain_submission(self, &document, &expanded)?;
            let settlement_knobs = provenant_run::settlement_policy::SettlementConfig::from_knobs(
                &config.knobs,
                self.context.roots.state.as_path(),
            );
            let mut writer_budgets = Vec::new();
            for node in &expanded.nodes {
                if node.access != provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
                    || !provenant_fabric_runtime::graph::when::holds(&node.when)
                {
                    continue;
                }
                let spec = node.attempt.clone().unwrap_or_default();
                let profile = config
                    .dispatch_profile(&spec.lane_knobs())
                    .map_err(|error| FacadeError::DispatchRefused {
                        fact: None,
                        disk: None,
                        reason: error.to_string(),
                    })?;
                let mut context = node.context.clone().unwrap_or_default();
                for (field, configured) in [
                    ("token_budget", spec.token_budget.is_some()),
                    ("on_budget", spec.on_budget.is_some()),
                    ("budget_grace", spec.budget_grace.is_some()),
                ] {
                    if configured {
                        context.budget_sources.insert(
                            field.into(),
                            spec.budget_sources
                                .get(field)
                                .copied()
                                .unwrap_or(provenant_records::Layer::Template),
                        );
                    }
                }
                let routes: Vec<Option<&provenant_fabric_runtime::graph::GraphRoute>> = node
                    .routes
                    .as_ref()
                    .map(|routes| routes.iter().map(Some).collect())
                    .unwrap_or_else(|| vec![node.route.as_ref()]);
                for route in routes {
                    let tier = route.and_then(|route| route.alias.as_deref());
                    let (token_budget, policy) = provenant_run::writer_budget_limits(
                        &run_config,
                        &self.context.identity.project.to_string(),
                        &profile,
                        Some(&context),
                        tier,
                    );
                    writer_budgets.push(json!({
                        "node": node.id,
                        "tier": tier,
                        "token_budget": token_budget,
                        "budget_policy": policy,
                        "gh_budget": provenant_run::gh_budget(&profile),
                        "ci_scope": provenant_run::ResolvedCiScope::resolve(&profile).receipt(),
                        "pr_policy": provenant_run::ResolvedPrPolicy::resolve(&profile).receipt(),
                        // The value a launch on this route seals, with the
                        // matched model policy as its route layer.
                        "settlement": provenant_run::settlement_policy::explain(
                            &settlement_knobs
                                .clone()
                                .with_route(&route_settlement_layer(&config, route)),
                        ),
                    }));
                }
            }
            let capability_gaps = provenant_cli::adapter_capabilities::explain_capability_gaps(
                &self.context.roots,
                &self.context.identity.project.to_string(),
                &config,
                &expanded.nodes,
            );
            let node_routes = self.explain_node_routes(&expanded.nodes);
            let source_admission = self.explain_source_admission(&expanded.nodes)?;
            let refused = source_admission
                .iter()
                .any(|row| row["status"] == "refused");
            let reused_writer_bases = reused_writer_bases
                .iter()
                .map(|(node, base)| {
                    json!({
                        "node": node,
                        "commit": base.as_ref().and_then(|base| base.commit.as_deref()),
                        "source": base.as_ref().map(|base| base.source.label()),
                    })
                })
                .collect::<Vec<_>>();
            return Ok(json!({
                "document":document, "skipped":skipped, "writer_budgets":writer_budgets,
                "capability_gaps": capability_gaps,
                "reused_writer_bases": reused_writer_bases,
                "node_routes": node_routes,
                "status": if refused { "refused" } else if source_admission.is_empty() { "admitted" } else { "pending_route" },
                "credential_admission": if source_admission.is_empty() { "not_applicable" } else { "pending_route" },
                "source_admission": source_admission, "warnings": warnings,
                "subject": subject,
                "read_source": read_source_report(default_source.as_ref(), &read_source),
                "writer_base": writer_base_report(writer_base.as_ref(), &writer_base_setting, writer_base_source),
                // The silence threshold is a host default with a named source,
                // just like the other settings an explanation reports.
                "stall": {
                    "default_threshold_seconds": provenant_records::DEFAULT_STALL_AFTER_SECONDS,
                    "providers": config.providers.iter().map(|(id, settings)| {
                        let field = format!("providers.{id}.stall_after_seconds");
                        let source = config.origins.get(&field)
                            .map(|origin| json!(origin))
                            .unwrap_or(json!(format!("compiled defaults ({field})")));
                        (id.clone(), json!({
                            "threshold_seconds": settings.stall_after_seconds,
                            "policy": settings.stall_policy.as_str(),
                            "source": source,
                        }))
                    }).collect::<serde_json::Map<String, Value>>(),
                },
                // Settlement knobs are sealed with the evidence, so an
                // explanation shows the same layers the holder will seal. The
                // host and project layers are here; each writer budget row
                // adds its route's model policy.
                "settlement": provenant_run::settlement_policy::explain(&settlement_knobs),
                "implement_plan": {
                    "value": input.pointer("/set/plan").or_else(|| input.pointer("/document/with/plan")).cloned().unwrap_or(json!(self.context.config.implement.plan)),
                    "source": if input.pointer("/set/plan").or_else(|| input.pointer("/document/with/plan")).is_some() { json!("parameter (--plan or --set)") } else {
                        config.origins.get("domains.fabric.implement.plan").map(|origin| json!(origin)).unwrap_or(json!("default"))
                    }
                },
                "max_parallel": {
                    "value": input.pointer("/set/max_parallel").or_else(|| input.pointer("/document/with/max_parallel")).cloned().unwrap_or(json!(self.context.config.batch.max_parallel)),
                    "source": if input.pointer("/set/max_parallel").or_else(|| input.pointer("/document/with/max_parallel")).is_some() { json!("parameter (--set)") } else {
                        config.origins.get("domains.fabric.batch.max_parallel").map(|origin| json!(origin)).unwrap_or(json!("default"))
                    }
                },
                "launch_settings": launch_settings_report(&flags, &read_source),
            }));
        }
        let mut result = match self.launch_submission(
            document,
            input,
            queue_launch.as_ref(),
            budget,
            cancel,
            announce,
        ) {
            // A concurrent launch bound this name to other input between the
            // choice and the bind. Choose again: the family now includes it.
            Err(FacadeError::Conflict)
                if strict_queue_id.is_none() && self.name_taken_by_other_input(id, &digest) =>
            {
                return self.submit_document(submitted, budget, cancel, announce);
            }
            result => result?,
        };
        let replayed = result["replayed"].as_bool().unwrap_or(false);
        if let Some(object) = result.as_object_mut() {
            object.remove("replayed");
        }
        if let Some(pin) = review_pin {
            result["review_subject"] = pin;
        }
        result["subject"] = json!(subject);
        result["warnings"] = json!(warnings);
        result["read_source"] = read_source_report(default_source.as_ref(), &read_source);
        result["writer_base"] = writer_base_report(
            writer_base.as_ref(),
            &writer_base_setting,
            writer_base_source,
        );
        result["launch_settings"] = launch_settings_report(&flags, &read_source);
        result["named"] = json!({
            "requested": requested, "graph": id, "replayed": replayed, "superseded": taken,
        });
        Ok(result)
    }

    /// Resolve the node a template or `use` launch runs before its identity is
    /// sealed. A writer's roles decide whether a read-only default source applies
    /// at all, and its base supplies the identity commit.
    fn template_launch(
        &self,
        input: &Value,
        raw: Option<&Value>,
        lane_document: Option<&Value>,
        route: Option<&RouteChoice>,
        context: &PatchContext<'_>,
        input_warnings: &mut Vec<String>,
    ) -> Result<TemplateLaunch, FacadeError> {
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
        if let Some(name) = input.get("template").filter(|value| !value.is_null()) {
            node["use"] = name.clone();
        }
        // The CLI brief is a node prompt; template parameters retain their names.
        for (field, node_field) in [("brief", "prompt"), ("over", "over")] {
            if let Some(value) = input.get(field).filter(|value| !value.is_null()) {
                node[node_field] = value.clone();
            }
        }
        if let Some(RouteChoice::Alias(alias)) = route {
            node["tier"] = json!(alias);
        }
        let template = context
            .library
            .resolve(node["use"].as_str().unwrap_or("task"))
            .map_err(graph_input_reason)?;
        // The same template the launch declared: review ranges are bound on
        // the host, so the node never carries its own subject or base.
        let review_launch = template.accepts_review_range();
        let bundled_smoke =
            node["use"] == "smoke" && context.library.layer("smoke") == Some(Layer::Default);
        // Launch controls bind to one leaf. A composite has no single
        // worktree to bind, so name the first control it cannot take.
        let first_control = LAUNCH_CONTROLS
            .iter()
            .find(|field| input.get(**field).is_some_and(|value| !value.is_null()));
        if template.is_composite()
            && !bundled_smoke
            && let Some(field) = first_control
        {
            return Err(FacadeError::invalid(InvalidInputDetail::Field(
                (*field).into(),
            )));
        }
        // The smoke template binds its controls to its write node only,
        // so they are bound onto a scratch node and carried as parameters.
        let mut smoke_writer = json!({});
        if bundled_smoke {
            for field in ["access", "write_scope"] {
                if input.get(field).is_some_and(|value| !value.is_null()) {
                    return Err(FacadeError::invalid(InvalidInputDetail::Field(
                        field.into(),
                    )));
                }
            }
        }
        bind_launch_controls(
            input,
            if bundled_smoke {
                &mut smoke_writer
            } else {
                &mut node
            },
            &self.context.project_root,
            &self.context.port_table,
        )?;
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
                    .ok_or_else(|| FacadeError::invalid(InvalidInputDetail::Field("set".into())))?
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
            fields.remove("prompt");
            fields.remove("tier");
        }
        // Node parameters supplied explicitly (including CLI --set) beat
        // document settings; template defaults are the final fallback.
        let bundled_batch =
            node["use"] == "batch" && context.library.layer("batch") == Some(Layer::Default);
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
        if bundled_smoke {
            smoke::configure(
                input,
                &mut parameters,
                &overrides,
                &smoke_writer,
                first_control.copied(),
                input_warnings,
            )?;
        }
        node["with"] = parameters;
        Ok(TemplateLaunch {
            node,
            integration_branch: integration_branch
                .as_ref()
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
    }

    /// Read-only route preview for `run --explain`. An exact node route or a
    /// policy's configured model resolves to one spelling; a pool is shown as
    /// its candidates, because the weighted pick, account and credential stay
    /// launch-only. Every value names the field, policy, pool or default that
    /// supplied it so pool routing can be checked before launch.
    fn explain_node_routes(
        &self,
        nodes: &[provenant_fabric_runtime::graph::GraphNode],
    ) -> Vec<Value> {
        use provenant_fabric_runtime::graph::{GraphRoute, GraphRouteSelector, NodeKind};
        // The provider check returns the route list together with the account
        // settings; this preview only needs the list's named routes and pools.
        let listed = provenant_cli::route::provider_check_routes(&self.context.roots)
            .ok()
            .map(|(routes, _accounts)| routes);
        let optional = |value: Option<String>| value.map_or(Value::Null, Value::String);
        let adapter_of = |model: &str| model.split_once('/').map(|(adapter, _)| adapter.to_owned());
        let mut rows = Vec::new();
        for node in nodes {
            if !provenant_fabric_runtime::graph::when::holds(&node.when)
                || node.composite.is_some()
                || node.condition.is_some()
                || node.decide.is_some()
                || node.loop_body.is_some()
                || node.land.is_some()
                || matches!(
                    node.kind,
                    Some(
                        NodeKind::Composite
                            | NodeKind::Gate
                            | NodeKind::Loop
                            | NodeKind::Decide
                            | NodeKind::Land
                            | NodeKind::Hygiene
                            | NodeKind::Publish
                    )
                )
            {
                continue;
            }
            let declared: Vec<Option<&GraphRoute>> = node
                .routes
                .as_ref()
                .map(|routes| routes.iter().map(Some).collect())
                .unwrap_or_else(|| vec![node.route.as_ref()]);
            for (index, route) in declared.into_iter().enumerate() {
                let Some(route) = route else {
                    rows.push(json!({
                        "node": node.id, "index": index,
                        "adapter": Value::Null, "model": Value::Null, "effort": Value::Null,
                        "sources": {"adapter":"default","model":"default","effort":"default"},
                        "selection": "default", "candidates": []
                    }));
                    continue;
                };
                let effort = route.effort.clone();
                let node_effort = effort.is_some();
                let (adapter, model, resolved_effort, sources, selection, candidates, route_name): (
                    Value,
                    Value,
                    Value,
                    Value,
                    &str,
                    Value,
                    Value,
                ) = match route.selector() {
                    GraphRouteSelector::Model(model) => (
                        optional(route.adapter.clone().or_else(|| adapter_of(model))),
                        Value::String(model.to_owned()),
                        optional(effort),
                        json!({
                            "adapter":"node field","model":"node field",
                            "effort": if node_effort { "node field" } else { "default" }
                        }),
                        "node field",
                        json!([]),
                        Value::Null,
                    ),
                    GraphRouteSelector::Adapter(name) => (
                        route
                            .adapter
                            .clone()
                            .map_or(Value::Null, Value::String),
                        Value::Null,
                        optional(effort),
                        json!({
                            "adapter":"node field","model":"node field",
                            "effort": if node_effort { "node field" } else { "default" }
                        }),
                        "node field",
                        json!([]),
                        Value::String(name.to_owned()),
                    ),
                    GraphRouteSelector::Alias(alias) => {
                        let alias = alias.unwrap_or_default();
                        match listed
                            .as_ref()
                            .and_then(|list| list.named_routes.get(alias))
                        {
                            Some(named) => {
                                let model = named.model.as_str().to_owned();
                                let policy_effort = effort.clone().or_else(|| named.effort.clone());
                                (
                                    adapter_of(&model).map_or(Value::Null, Value::String),
                                    Value::String(model.clone()),
                                    optional(policy_effort),
                                    json!({
                                        "adapter":"policy","model":"policy",
                                        "effort": if node_effort { "node field" }
                                            else if named.effort.is_some() { "policy" }
                                            else { "default" },
                                        "origin": named.source.clone(),
                                    }),
                                    "policy",
                                    json!([model]),
                                    Value::Null,
                                )
                            }
                            None => match listed.as_ref().and_then(|list| list.pools.get(alias)) {
                                Some(pool) => {
                                    let models: Vec<String> = pool
                                        .iter()
                                        .map(|member| member.model.as_str().to_owned())
                                        .collect();
                                    let common_effort = pool
                                        .first()
                                        .and_then(|member| member.effort.clone())
                                        .filter(|value| {
                                            pool.iter()
                                                .all(|member| member.effort.as_ref() == Some(value))
                                        });
                                    let candidates: Vec<Value> = pool
                                        .iter()
                                        .map(|member| {
                                            let model = member.model.as_str();
                                            let member_effort = effort
                                                .clone()
                                                .or_else(|| member.effort.clone());
                                            json!({
                                                "model": model,
                                                "adapter": adapter_of(model),
                                                "effort": member_effort,
                                                "sources": {
                                                    "model": "pool",
                                                    "adapter": "pool",
                                                    "effort": if node_effort { "node field" }
                                                        else if member.effort.is_some() { "pool" }
                                                        else { "default" }
                                                }
                                            })
                                        })
                                        .collect();
                                    let common_adapter = models
                                        .first()
                                        .and_then(|model| adapter_of(model))
                                        .filter(|adapter| {
                                            models.iter().all(|model| {
                                                adapter_of(model).as_deref()
                                                    == Some(adapter.as_str())
                                            })
                                        });
                                    (
                                        common_adapter.map_or(Value::Null, Value::String),
                                        Value::Null,
                                        optional(effort.clone().or(common_effort)),
                                        json!({
                                            "adapter":"pool","model":"pool",
                                            "effort": if node_effort { "node field" }
                                                else if pool.iter().any(|member| member.effort.is_some()) { "pool" }
                                                else { "default" },
                                            "origin": format!("pool {alias}"),
                                        }),
                                        "pool",
                                        json!(candidates),
                                        Value::Null,
                                    )
                                }
                                None => (
                                    Value::Null,
                                    Value::Null,
                                    optional(effort),
                                    json!({
                                        "adapter":"default","model":"default",
                                        "effort": if node_effort { "node field" } else { "default" }
                                    }),
                                    "default",
                                    json!([]),
                                    Value::Null,
                                ),
                            },
                        }
                    }
                };
                rows.push(json!({
                    "node": node.id,
                    "index": index,
                    "adapter": adapter,
                    "model": model,
                    "effort": resolved_effort,
                    "sources": sources,
                    "selection": selection,
                    "candidates": candidates,
                    "route_name": route_name,
                }));
            }
        }
        rows
    }

    /// Inspect source and state admission without preparing a provider. The
    /// route-specific credential decision remains pending until selection.
    fn explain_source_admission(
        &self,
        nodes: &[provenant_fabric_runtime::graph::GraphNode],
    ) -> Result<Vec<Value>, FacadeError> {
        use provenant_cli::run::{SourceAdmissionMode, admit_source_root};
        let mut rows = Vec::new();
        for node in nodes {
            if !provenant_fabric_runtime::graph::when::holds(&node.when)
                || node.composite.is_some()
                || node.condition.is_some()
                || node.decide.is_some()
            {
                continue;
            }
            let resolve = |path: &str| {
                let path = Path::new(path);
                if path.is_absolute() {
                    path.to_owned()
                } else {
                    self.cwd.join(path)
                }
            };
            // Managed writer worktrees live under the project's primary root,
            // even when the launch runs from a linked checkout.
            let writer_root = self.context.project_root.as_path();
            let resolve_worktree = |path: &str| {
                let path = Path::new(path);
                if path.is_absolute() {
                    path.to_owned()
                } else {
                    writer_root.join(path)
                }
            };
            // A peer-placed writer is provisioned on the peer from a branch
            // and a base ref. It has no local path to admit: resolving its
            // empty worktree against this checkout would treat the caller's
            // root as the writer and refuse it for the caller's state root
            // (#4103). Report the peer's own provisioned worktree as proposed
            // and leave credential admission to the peer's launch.
            if self
                .node_host(node)?
                .is_some_and(|host| host != self.context.identity.host)
                && node.access == AccessMode::WorktreeWrite
                && node
                    .attempt
                    .as_ref()
                    .is_some_and(|attempt| attempt.new_worktree.is_some())
            {
                let label = node
                    .attempt
                    .as_ref()
                    .and_then(|attempt| attempt.host.as_deref())
                    .unwrap_or("peer");
                let relative = node
                    .attempt
                    .as_ref()
                    .and_then(|attempt| attempt.new_worktree.as_ref())
                    .and_then(|worktree| {
                        self.context
                            .port_table
                            .managed_writer_path(&worktree.branch)
                            .ok()
                    })
                    .unwrap_or_else(|| PathBuf::from("writer"));
                let path = PathBuf::from(label).join(relative);
                rows.push(json!({"node":node.id, "host":label, "path":path,
                    "status":"pending_registration", "credential_admission":"pending_route",
                    "warnings":[]}));
                continue;
            }
            let access = provenant_cli::Access {
                mode: None,
                worktree: (node.access
                    == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite)
                    .then(|| node.worktree.as_deref().map(resolve_worktree))
                    .flatten(),
                read_root: node.read_roots.iter().map(|path| resolve(path)).collect(),
                write_root: node
                    .attempt
                    .iter()
                    .flat_map(|attempt| &attempt.write_roots)
                    .map(|path| resolve(path))
                    .collect(),
                attach: Vec::new(),
                network: None,
                capability: Vec::new(),
                no_capability: Vec::new(),
            };
            if let Err(error) =
                provenant_cli::run::lane_access(&access, self.context.roots.state.as_path())
            {
                rows.push(json!({"node":node.id,"status":"refused",
                    "code":"dispatch_refused","reason":error.message}));
                continue;
            }
            let selected = node.worktree.as_deref().or_else(|| {
                node.attempt
                    .as_ref()
                    .and_then(|attempt| attempt.directory.as_deref())
            });
            let path = selected.map_or_else(
                || self.cwd.clone(),
                |path| {
                    let path = Path::new(path);
                    if path.is_absolute() {
                        path.to_owned()
                    } else if node.access
                        == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
                    {
                        writer_root.join(path)
                    } else {
                        self.cwd.join(path)
                    }
                },
            );
            // A proposed writer does not yet have a registered worktree.
            // Inspect its declared boundary now; registration is verified once
            // the worktree owner provisions it at launch.
            let proposed = (node.provision_worktree
                || node
                    .attempt
                    .as_ref()
                    .is_some_and(|attempt| attempt.new_worktree.is_some()))
                && !path.exists();
            let mode = if proposed {
                SourceAdmissionMode::Proposed
            } else if node.source.is_none() {
                SourceAdmissionMode::Live
            } else {
                SourceAdmissionMode::Registered
            };
            match admit_source_root(&self.cwd, self.context.roots.state.as_path(), &path, mode) {
                Ok(admission) => {
                    let warnings: Vec<_> = admission
                        .warnings
                        .iter()
                        .map(|warning| json!({"path":warning.path, "source":warning.source}))
                        .collect();
                    rows.push(json!({"node":node.id, "path":admission.path,
                        "status":if proposed { "pending_registration" } else { "pending_route" },
                        "credential_admission":"pending_route", "warnings":warnings}));
                }
                Err(error) => rows.push(json!({"node":node.id,"path":path,"status":"refused",
                    "code":error.code,"reason":error.reason})),
            }
        }
        Ok(rows)
    }

    /// Whether graph `id` now exists with a launch digest other than `digest`.
    fn name_taken_by_other_input(&self, id: &str, digest: &str) -> bool {
        FileJournal::execution(&self.context.roots.state, self.context.identity.project, id)
            .ok()
            .flatten()
            .is_some_and(|execution| execution.document.replay_digest() != Some(digest))
    }

    fn launch_submission(
        &self,
        document: GraphDocument,
        input: &Value,
        queue_launch: Option<&super::queue_registry::AuthorisedQueueLaunch>,
        budget: WaitBudget,
        cancel: &CancelToken,
        announce: Option<super::graph_execution::Announce<'_>>,
    ) -> Result<Value, FacadeError> {
        let scratch = self
            .prepare_scratch(cancel)
            .map_err(scratch_prepare_error)?;
        let path = publish_graph_document(&scratch, &encode(&document)?)
            .map_err(|_| FacadeError::InvalidInput { detail: None })?;
        let mut replayed = false;
        let run = self.graph_launch_result(
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
            announce,
            Some(&mut replayed),
            None,
            queue_launch,
        )?;
        let (run, wait_observation, observation_wait, remote_read_allowed) = if replayed {
            (run, None, None, true)
        } else {
            self.wait_document(run, input, budget, cancel)?
        };
        let timed_out = !replayed
            && input.get("wait_seconds").and_then(Value::as_u64).is_some()
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
        if !rows.is_empty()
            && remote_read_allowed
            && !cancel.is_cancelled()
            && observation_wait
                .is_none_or(|wait| wait.remaining(self.clock.mono_now()).as_micros() > 0)
        {
            super::unified::enrich_status_rows_within(
                &mut rows,
                &self.context,
                &self.clock,
                cancel,
                observation_wait.unwrap_or(budget),
            )?;
        }
        let mut result = encode(run)?;
        result["replayed"] = json!(replayed);
        report_launch_refusals(&mut result, &rows);
        result["timed_out"] = json!(timed_out);
        if let Some(observation) = wait_observation {
            result["observation"] = json!(observation);
        }
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
        let patch: Patch = decode_input(input["patch"].clone(), "patch")?;
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
        if super::queue_controls::drop_fence_active(&checkpoint)? {
            return Err(graph_input_reason("dropped queue items cannot be edited"));
        }
        let library = self.template_library(None)?;
        let resolver = self.launch_resolvers(Some(&encode(&execution.document)?))?;
        let catalogue = provenant_cli::route::effective_catalogue(&self.context.roots).ok();
        let admitted = patch::admit_patch(
            &mut LiveGraph {
                document: &execution.document,
                checkpoint: &mut checkpoint,
            },
            None,
            &patch,
            &PatchContext {
                catalogue: catalogue.as_ref(),
                library: &library,
                resolvers: &resolver,

                journal: None,
                limits: &self.context.config.graph.limits,
                worktree_layout: self.context.port_table.worktree_layout.clone(),
            },
        )
        .map_err(graph_patch_error)?;
        for expansion in &mut checkpoint.expansions {
            for node in &mut expansion.nodes {
                let mut value = encode(&*node)?;
                self.confine_node(&mut value)?;
                // Expansion nodes carry computed topology outside their ordinary
                // serialisation. Bind only access fields, preserving that topology.
                bind_read_roots(node, &value)?;
            }
        }
        provenant_fabric_runtime::graph::admit_candidate(
            &execution.document,
            Some(&checkpoint),
            &[],
            &self.context.config.graph.limits,
            &self.context.config.graph.budget,
            execution.requester_path.as_deref(),
        )
        .map_err(FacadeError::graph_candidate)?;
        // save writes patch events before publishing the checkpoint, under this lock.
        journal.save(&checkpoint).map_err(graph_journal_error)?;
        Ok(
            json!({"graph_id":id,"revision":admitted.revision,"replayed":admitted.replayed,"added":admitted.added}),
        )
    }

    pub(super) fn retry_document(
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
            .map_err(|_| FacadeError::invalid(InvalidInputDetail::Field("extend".into())))?
            .unwrap_or_default();
        if !extend.is_empty()
            && (target.node.is_some() || input.get("brief").is_some_and(|value| !value.is_null()))
        {
            return Err(FacadeError::invalid(InvalidInputDetail::Field(
                "node".into(),
            )));
        }
        let id = target.graph.clone();
        let run = self.graph_retry(
            GraphInput::Resume {
                extra: Default::default(),
                document: id.into(),
                extend,
            },
            budget,
            cancel,
            &mut target,
            input,
        )?;
        target.attempt = None;
        self.control_receipt(run, &target, input, budget, cancel, false)
    }

    pub(super) fn prepare_retry_with_journal(
        &self,
        target: &mut super::unified::GraphTarget,
        input: &Value,
        document: &GraphDocument,
        checkpoint: &mut provenant_fabric_runtime::graph::GraphCheckpoint,
        journal: &mut FileJournal,
    ) -> Result<(), FacadeError> {
        self.prepare_retry_inner(target, input, document, checkpoint, Some(journal))
    }

    fn prepare_retry_inner(
        &self,
        target: &mut super::unified::GraphTarget,
        input: &Value,
        document: &GraphDocument,
        checkpoint: &mut provenant_fabric_runtime::graph::GraphCheckpoint,
        mut journal: Option<&mut FileJournal>,
    ) -> Result<(), FacadeError> {
        if super::queue_controls::drop_fence_active(checkpoint)? {
            return Err(graph_input_reason("dropped queue items cannot be retried"));
        }
        if input.get("queue_accept_retry").is_some() && target.node.is_none() {
            return Err(graph_input_reason(
                "accept gate retry requires one exact node target",
            ));
        }
        if provenant_redact::mode().masks_egress()
            && let Some(brief) = input.get("brief").and_then(Value::as_str)
        {
            match provenant_redact::matches_credential_value_for_retry(brief.as_bytes()) {
                Ok(true) => warn_operator(
                    "declared credential value rule: retry brief holds a value matching a known credential",
                ),
                Err(_) => warn_operator(
                    "declared credential value rule: retry brief credential check failed",
                ),
                Ok(false) => {}
            }
        }
        let id = target.graph.clone();
        let brief_given = input.get("brief").is_some_and(|value| !value.is_null());
        // Keep the no-op guard under the retry journal lock, alongside the
        // staged retry mutation.
        let extensions_empty = input
            .get("extend")
            .is_none_or(|extensions| extensions.as_array().is_some_and(Vec::is_empty));
        // The cancel flag alone restores nothing: only nodes cancelled out of a
        // live state (Skipped with the cancel reason) are resumable.
        let has_resumable_work = checkpoint.pause.is_some()
            || checkpoint.nodes.iter().any(|node| {
                node.expansion_pause.is_some()
                    || node.loop_pause.is_some()
                    || (node.state == provenant_fabric_runtime::graph::NodeState::Skipped
                        && node.skip_reason.as_deref() == Some("graph cancelled"))
                    || matches!(
                        node.state,
                        provenant_fabric_runtime::graph::NodeState::Pending
                            | provenant_fabric_runtime::graph::NodeState::Running
                    )
            });
        // A settled graph with nothing to resume has no graph-wide retry: the
        // chair must name a node. A graph of exactly one node names it without
        // guessing; a wider graph still needs an explicit node.
        let settled_without_target = target.node.is_none()
            && !brief_given
            && extensions_empty
            && !has_resumable_work
            && checkpoint.nodes.iter().all(|node| node.state.settled());
        if settled_without_target && checkpoint.nodes.len() != 1 {
            let node = checkpoint.nodes.iter().find(|node| {
                matches!(
                    node.state,
                    provenant_fabric_runtime::graph::NodeState::Failed
                        | provenant_fabric_runtime::graph::NodeState::Blocked
                )
            });
            return Err(FacadeError::invalid(InvalidInputDetail::RetryNode {
                graph: id.clone(),
                node: node.map_or_else(|| "NODE".into(), |node| node.id.clone()),
            }));
        }
        if target.node.is_some() || brief_given || settled_without_target {
            // A brief belongs to one node, and a settled graph without a target
            // retries its only node. A graph of one node names it; otherwise the
            // chair must say which.
            let node = match target.node.clone() {
                Some(node) => node,
                None => match checkpoint.nodes.as_slice() {
                    [only] => only.id.clone(),
                    _ => {
                        return Err(FacadeError::invalid(InvalidInputDetail::GraphRelayCause(
                            provenant_fabric_runtime::coordination::facade::GraphRelayCause::RetryBriefNeedsNode,
                        )));
                    }
                },
            };
            if let Some(marker) = input.get("queue_accept_retry") {
                if input.get("queue_control").is_some() || brief_given || !extensions_empty {
                    return Err(graph_input_reason(
                        "accept gate retry cannot carry another control, brief, or extension",
                    ));
                }
                let (operation_key, operation_digest) =
                    super::queue_controls::authorise_accept_gate_retry(
                        checkpoint, &id, &node, marker,
                    )?;
                let journal = journal.as_deref_mut().ok_or(FacadeError::StateCorrupt)?;
                journal.save(checkpoint).map_err(graph_journal_error)?;
                let library = self.template_library(None)?;
                let resolver = self.launch_resolvers(Some(&encode(document)?))?;
                patch::retry_node_and_dependants_with_key(
                    &mut LiveGraph {
                        document,
                        checkpoint,
                    },
                    &node,
                    None,
                    &PatchContext {
                        catalogue: None,
                        library: &library,
                        resolvers: &resolver,
                        journal: None,
                        limits: &self.context.config.graph.limits,
                        worktree_layout: self.context.port_table.worktree_layout.clone(),
                    },
                    &operation_key,
                    &operation_digest,
                )
                .map_err(retry_patch_error)?;
                target.node = Some(node);
                journal.save(checkpoint).map_err(graph_journal_error)?;
                return Ok(());
            }
            if let Some(control) = input.get("queue_control") {
                let metadata = super::queue_controls::decode_metadata(control)?;
                if !matches!(
                    metadata.action,
                    super::queue_controls::Action::Retry | super::queue_controls::Action::Fix
                ) {
                    return Err(graph_input_reason(
                        "queue retry metadata requires the retry or fix action",
                    ));
                }
                let payload = super::queue_controls::without_queue_control(input)?;
                let operation_digest = super::queue_controls::control_digest(
                    &metadata,
                    &id,
                    Some(&node),
                    None,
                    &payload,
                )?;
                let (terminal, _) = super::queue_controls::reserve_retry_control(
                    checkpoint, document, &id, &node, &metadata, &payload,
                )?;
                let journal = journal.ok_or(FacadeError::StateCorrupt)?;
                if terminal {
                    journal.save(checkpoint).map_err(graph_journal_error)?;
                    return Err(FacadeError::Conflict);
                }
                journal.save(checkpoint).map_err(graph_journal_error)?;
                let library = self.template_library(None)?;
                let resolver = self.launch_resolvers(Some(&encode(document)?))?;
                patch::retry_node_and_dependants_with_key(
                    &mut LiveGraph {
                        document,
                        checkpoint,
                    },
                    &node,
                    input.get("brief").and_then(Value::as_str),
                    &PatchContext {
                        catalogue: None,
                        library: &library,
                        resolvers: &resolver,
                        journal: None,
                        limits: &self.context.config.graph.limits,
                        worktree_layout: self.context.port_table.worktree_layout.clone(),
                    },
                    &metadata.operation_id.to_string(),
                    &operation_digest,
                )
                .map_err(retry_patch_error)?;
                target.node = Some(node);
                journal.save(checkpoint).map_err(graph_journal_error)?;
                return Ok(());
            }
            // A previous retry may have committed its reset before its resume
            // failed. Resume that already admitted retry rather than archive
            // its predecessor a second time.
            let interrupted = !brief_given
                && checkpoint.nodes.iter().any(|progress| {
                    progress.id == node
                        && progress.state == provenant_fabric_runtime::graph::NodeState::Pending
                        && progress.attempts.is_empty()
                        && progress.retry_generation.is_some()
                        && !progress.previous_attempts.is_empty()
                })
                && checkpoint
                    .expansions
                    .iter()
                    .rev()
                    .find_map(|expansion| expansion.patch.as_ref())
                    .is_some_and(|patch| {
                        patch.reason.as_deref() == Some(format!("retry node {node}").as_str())
                    });
            if interrupted {
                // A pre-fix retry could remove the owner of an expansion
                // pause while leaving the graph pause behind. Preserve any
                // other pause for the normal resume path.
                if checkpoint.pause_cause()
                    == Some(provenant_records::pause::PauseCause::InvalidPlanExpansion)
                    && !checkpoint.nodes.iter().any(|progress| {
                        matches!(&progress.expansion_cause,
                            Some(provenant_fabric_runtime::graph::NodeDecisionCause::PlanExpansionPaused { active: true, .. }))
                    })
                {
                    checkpoint.pause = None;
                }
            } else {
                let library = self.template_library(None)?;
                let resolver = self.launch_resolvers(Some(&encode(document)?))?;
                patch::retry_node_and_dependants(
                    &mut LiveGraph {
                        document,
                        checkpoint,
                    },
                    &node,
                    input.get("brief").and_then(Value::as_str),
                    &PatchContext {
                        catalogue: None,
                        library: &library,
                        resolvers: &resolver,

                        journal: None,
                        limits: &self.context.config.graph.limits,
                        worktree_layout: self.context.port_table.worktree_layout.clone(),
                    },
                )
                .map_err(retry_patch_error)?;
            }
            target.node = Some(node);
        }
        Ok(())
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
        let cancellation_request = run.checkpoint.cancellation_request_receipt.take();
        let mut timed_out = false;
        let mut observation_report = None;
        let mut observation_wait = None;
        let mut remote_read_allowed = true;
        if let Some(seconds) = input.get("wait_seconds").and_then(Value::as_u64) {
            let wait = WaitBudget::cli(self.clock.mono_now(), seconds)
                .map_err(|_| wait_seconds_invalid())?
                .cap_at(budget.deadline().as_mono_time());
            let cadence = self.wait_cadence()?;
            let mut observation = self.local_graph_observation(&target.graph, cadence);
            let remote_target = if stopping {
                target.clone()
            } else {
                super::unified::GraphTarget {
                    graph: target.graph.clone(),
                    node: None,
                    attempt: None,
                }
            };
            let (observed, history) = wait_graph_updates(
                run,
                wait,
                &self.clock,
                cancel,
                &mut observation,
                Some(self.context.identity.host),
                stopping.then_some(target),
                false,
                |run| control_receipt_unfinished(run, target, stopping),
                |_| {
                    // The original stop request is durable. Rechecking status
                    // must not append another request while its owner is busy.
                    self.graph(
                        GraphInput::Status {
                            extra: Default::default(),
                            id: target.graph.clone(),
                        },
                        wait,
                        cancel,
                    )
                },
                |run| self.observe_remote_graph_attempts(run, &remote_target, wait, cancel),
            )?;
            run = observed;
            observation_wait = Some(wait);
            remote_read_allowed = !history.reader_woken;
            timed_out = control_receipt_unfinished(&run, target, stopping)
                && (wait.remaining(self.clock.mono_now()).as_micros() == 0
                    || cancel.is_cancelled());
            observation_report = Some(history.report(&observation));
        }
        let mut rows = super::unified::status_rows(&run, target);
        if remote_read_allowed
            && !cancel.is_cancelled()
            && observation_wait
                .is_none_or(|wait| wait.remaining(self.clock.mono_now()).as_micros() > 0)
        {
            super::unified::enrich_status_rows_within(
                &mut rows,
                &self.context,
                &self.clock,
                cancel,
                observation_wait.unwrap_or(budget),
            )?;
        }
        let mut receipt = json!({"rows":rows,"timed_out":timed_out});
        // The commit a read-only lane reads is already bound on its node; show
        // it on status as well, so the launch result and later status agree.
        let read_commits: Vec<Value> = run
            .checkpoint
            .nodes
            .iter()
            .filter_map(|node| match (&node.source_binding, &node.source_commit) {
                (
                    Some(provenant_fabric_runtime::graph::SourceBinding::Commit { .. }),
                    Some(commit),
                ) => Some(json!({"node":node.id,"commit":commit})),
                _ => None,
            })
            .collect();
        if !read_commits.is_empty() {
            receipt["read_source"] = json!({"commits":read_commits});
        }
        if let Some(observation) = observation_report {
            receipt["observation"] = json!(observation);
        }
        if let Some(request_id) = cancellation_request
            .as_ref()
            .map(|receipt| receipt.request_id.as_str())
        {
            let pending = FileJournal::cancellation_request_pending(
                &self.context.roots.state,
                self.context.identity.project,
                &target.graph,
                request_id,
            )
            .map_err(graph_journal_error)?;
            receipt["cancellation_requested"] = json!({
                "request_id": request_id,
                "acknowledged": !pending,
                "pending_acknowledgement": pending,
            });
        }
        Ok(receipt)
    }

    fn wait_cadence(&self) -> Result<provenant_cli::wait::LocalCadence, FacadeError> {
        crate::status_wait::local_cadence(&self.context.roots)
    }

    fn local_graph_observation(
        &self,
        graph: &str,
        cadence: provenant_cli::wait::LocalCadence,
    ) -> crate::status_wait::LocalObservation {
        if self.context.in_lane {
            return crate::status_wait::LocalObservation::paced(
                cadence,
                "lane graph state is observed through the host relay without a local event log",
            );
        }
        match provenant_fabric_runtime::graph::FileJournal::observation_paths(
            &self.context.roots.state,
            self.context.identity.project,
            graph,
        ) {
            Ok(paths) => crate::status_wait::LocalObservation::subscribe(&paths, cadence),
            Err(error) => crate::status_wait::LocalObservation::paced(
                cadence,
                format!("cannot resolve graph observation paths: {error}"),
            ),
        }
    }

    fn wait_document(
        &self,
        run: provenant_fabric_runtime::graph::GraphRun,
        input: &Value,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<
        (
            provenant_fabric_runtime::graph::GraphRun,
            Option<String>,
            Option<WaitBudget>,
            bool,
        ),
        FacadeError,
    > {
        let Some(seconds) = input.get("wait_seconds").and_then(Value::as_u64) else {
            return Ok((run, None, None, true));
        };
        let now = self.clock.mono_now();
        let deadline = now
            .checked_add(budget.remaining(now))
            .map_err(|_| wait_seconds_invalid())?;
        let wait = WaitBudget::cli(now, seconds)
            .map_err(|_| wait_seconds_invalid())?
            .cap_at(deadline);
        let cadence = self.wait_cadence()?;
        let mut observation = self.local_graph_observation(&run.checkpoint.graph_id, cadence);
        let (run, observation_report, remote_read_allowed) = wait_submission(
            run,
            wait,
            &self.clock,
            cancel,
            &mut observation,
            Some(self.context.identity.host),
            |id| {
                let _ = super::unified::graph_wait::wake_graph_for_reader(
                    self,
                    id,
                    crate::fabric::progress::WakeReason::InlineRun,
                    wait,
                    cancel,
                );
                self.graph(
                    GraphInput::Status {
                        extra: Default::default(),
                        id: id.to_owned(),
                    },
                    wait,
                    cancel,
                )
            },
            |run| {
                self.observe_remote_graph_attempts(
                    run,
                    &super::unified::GraphTarget {
                        graph: run.checkpoint.graph_id.clone(),
                        node: None,
                        attempt: None,
                    },
                    wait,
                    cancel,
                )
            },
        )?;
        Ok((
            run,
            Some(observation_report),
            Some(wait),
            remote_read_allowed,
        ))
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
            .map_err(|_| FacadeError::invalid(InvalidInputDetail::Field("read_roots".into())))?;
    }
    if let Some(plan) = value.get("plan") {
        node.plan = Some(
            serde_json::from_value(plan.clone())
                .map_err(|_| FacadeError::invalid(InvalidInputDetail::Field("plan".into())))?,
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

/// Type one `--set` value by the declaration of the template it binds.
fn typed_binding(
    template: &provenant_fabric_runtime::graph::template::Template,
    template_name: &str,
    name: &str,
    value: &Value,
) -> Result<Value, FacadeError> {
    template
        .type_binding(template_name, name, value.clone())
        .map_err(|refusal| FacadeError::invalid(InvalidInputDetail::Parameter(Box::new(refusal))))
}

/// The host digest of a launch: everything the caller chose except the name and
/// how long to wait, the effective templates it runs, and the commit the launch
/// started from. Equal digests are the same launch.
pub(super) fn submission_digest(
    input: &Value,
    source_commit: Option<&str>,
    templates: Option<&Value>,
) -> Result<String, FacadeError> {
    let mut submission = input.clone();
    if let Some(fields) = submission.as_object_mut() {
        for field in ["explain", "wait_seconds", "name"] {
            fields.remove(field);
        }
    }
    let mut identity = json!({"submission": submission, "source_commit": source_commit});
    if let Some(templates) = templates {
        identity["templates"] = templates.clone();
    }
    provenant_records::CanonicalV1::record_digest("graph-replay", 1, &identity)
        .map(|digest| digest.to_string())
        .map_err(|_| FacadeError::StateCorrupt)
}

/// The resolved body of the launched template and of every template it or the
/// submitted document reaches through `use`, so an edited template file is a
/// different launch even under the same name. A name that does not resolve is
/// kept as `null`; compilation reports it.
fn effective_templates(library: &TemplateLibrary, root: &str, input: &Value) -> Value {
    fn uses(value: &Value, into: &mut Vec<String>) {
        match value {
            Value::Object(fields) => {
                for (key, value) in fields {
                    match (key.as_str(), value) {
                        ("use", Value::String(name)) => into.push(name.clone()),
                        _ => uses(value, into),
                    }
                }
            }
            Value::Array(values) => values.iter().for_each(|value| uses(value, into)),
            _ => {}
        }
    }
    let mut pending = vec![root.to_owned()];
    uses(input, &mut pending);
    let mut resolved = serde_json::Map::new();
    while let Some(name) = pending.pop() {
        if resolved.contains_key(&name) {
            continue;
        }
        let body = library
            .resolve(&name)
            .ok()
            .and_then(|template| serde_json::to_value(template).ok())
            .unwrap_or(Value::Null);
        uses(&body, &mut pending);
        resolved.insert(name, body);
    }
    Value::Object(resolved)
}

/// The roles a `--route` map can name.
const ROUTE_ROLES: [&str; 5] = ["writer", "review", "synthesis", "task", "plan"];

/// What `--route` selected: one tier for every launched node, or a tier per role.
/// A role the map leaves out keeps the tier its template declares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RouteChoice {
    Alias(String),
    Roles(BTreeMap<String, String>),
}

impl RouteChoice {
    /// Read `ROUTE`, `role=tier,role=tier` or a JSON object of roles.
    pub(super) fn parse(value: &Value) -> Result<Self, FacadeError> {
        let invalid = || FacadeError::invalid(InvalidInputDetail::Field("route".into()));
        let mut roles = BTreeMap::new();
        match value {
            Value::String(text) if !text.contains('=') => {
                let alias = text.trim();
                return if alias.is_empty() {
                    Err(invalid())
                } else {
                    Ok(Self::Alias(alias.to_owned()))
                };
            }
            Value::String(text) => {
                for pair in text.split(',') {
                    let (role, tier) = pair.split_once('=').ok_or_else(invalid)?;
                    roles.insert(role.trim().to_owned(), tier.trim().to_owned());
                }
            }
            Value::Object(fields) => {
                for (role, tier) in fields {
                    roles.insert(role.clone(), tier.as_str().ok_or_else(invalid)?.to_owned());
                }
            }
            _ => return Err(invalid()),
        }
        if roles.is_empty() || roles.values().any(String::is_empty) {
            return Err(invalid());
        }
        if let Some(role) = roles
            .keys()
            .find(|role| !ROUTE_ROLES.contains(&role.as_str()))
        {
            return Err(FacadeError::DispatchRefused {
                fact: None,
                disk: None,
                reason: format!(
                    "unknown route role `{role}`; valid roles: {}",
                    ROUTE_ROLES.join(", ")
                ),
            });
        }
        Ok(Self::Roles(roles))
    }

    /// The tier a node of `role` takes, `None` to keep its declared tier.
    pub(super) fn tier_for(&self, role: &str) -> Option<&str> {
        match self {
            Self::Alias(alias) => Some(alias),
            Self::Roles(roles) => roles.get(role).map(String::as_str),
        }
    }

    fn tiers(&self) -> Vec<&str> {
        match self {
            Self::Alias(alias) => vec![alias],
            Self::Roles(roles) => roles.values().map(String::as_str).collect(),
        }
    }
}

/// The route an envelope or its document asks for: `route` on the envelope, else
/// the document's own `tier`.
fn route_choice(input: &Value, raw: Option<&Value>) -> Result<Option<RouteChoice>, FacadeError> {
    input
        .get("route")
        .filter(|value| !value.is_null())
        .or_else(|| {
            raw.and_then(|document| document.get("tier"))
                .filter(|value| !value.is_null())
        })
        .map(RouteChoice::parse)
        .transpose()
}

#[path = "interface_route.rs"]
mod route;
#[path = "interface_smoke.rs"]
mod smoke;
use route::override_tier;

fn add(row: &mut Value, key: &str, value: u64) {
    row[key] = json!(row[key].as_u64().unwrap_or(0).saturating_add(value));
}

#[cfg(test)]
#[path = "interface_queue_config_tests.rs"]
mod queue_config_tests;

#[cfg(test)]
#[path = "interface_writer_source_tests.rs"]
#[allow(
    dead_code,
    unused_imports,
    reason = "the custom worktree driver omits libtest functions"
)]
mod writer_source_tests;

#[cfg(test)]
#[path = "interface_tests.rs"]
#[allow(
    dead_code,
    unused_imports,
    reason = "the custom worktree driver omits libtest functions"
)]
mod tests;

/// Reports a launch warning for the operator; a warning never blocks a launch.
pub(super) fn warn_operator(warning: impl std::fmt::Display) {
    provenant_records::warning::warn(
        provenant_records::warning::Warning::new(
            provenant_records::warning::LAUNCH_ADVISORY,
            warning.to_string(),
            provenant_records::warning::detected_source(
                provenant_records::warning::LAUNCH_ADVISORY,
            ),
        )
        .once_per_change(),
    );
}

/// The `lanes.settlement.*` route layer the model policy gives a concrete
/// route, as launch would seal it. A tier alias or an unparsable model has no
/// concrete policy yet, so it adds nothing. Rules match against the bundled
/// catalogue here; a rule keyed on a discovered-only model shows at launch.
fn route_settlement_layer(
    config: &provenant_config::ResolvedConfig<provenant_cli::domains::Domains>,
    route: Option<&provenant_fabric_runtime::graph::GraphRoute>,
) -> Vec<provenant_records::settlement::policy::RouteLayerValue> {
    let Some(route) = route else {
        return Vec::new();
    };
    let label = match (route.adapter.as_deref(), route.model.as_deref()) {
        (_, Some(model)) if model.contains('/') => model.to_owned(),
        (Some(adapter), Some(model)) => format!("{adapter}/{model}"),
        _ => return Vec::new(),
    };
    let (Ok(model), Ok(catalogue), Ok(now)) = (
        provenant_routing::ModelId::parse(&label),
        provenant_routing::Catalogue::bundled(),
        provenant_core::Clock::wall_now(&provenant_os::clock::SystemClock),
    ) else {
        return Vec::new();
    };
    config
        .domains
        .routing
        .model_policy
        .effective(&catalogue, &model, route.account_key.as_deref(), now)
        .policy
        .lanes
        .route_layer()
}
