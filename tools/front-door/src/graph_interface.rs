//! CLI documents for the shared graph service.
use crate::fabric;
use provenant_cli::{Edit, Globals, GraphRunArgs, Templates};
use provenant_confinement::worktrees::WorkerObserver;
use provenant_core::{CancelToken, Clock, WaitBudget};
use provenant_records::{CommandResult, Outcome};
use provenant_routing::BareModelError;
use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;

fn document(path: &Path) -> Result<Value, Box<provenant_records::diagnostic::Diagnostic>> {
    let text = std::fs::read_to_string(path).map_err(|_| file_input_error(path))?;
    decode(&text).map_err(|_| cli_input_error("graph document is invalid"))
}
fn decode(text: &str) -> Result<Value, String> {
    serde_json::from_str(text)
        .or_else(|_| toml_edit::de::from_str(text))
        .map_err(|error| error.to_string())
}
fn response_outcome(command: &str, data: &Value) -> Outcome {
    if data["timed_out"] == true {
        return Outcome::Waiting;
    }
    if command == "run" {
        match data["status"].as_str() {
            Some("complete") if data["outcome"] == "failed" => return Outcome::Failed,
            Some("cancelled") => return Outcome::Cancelled,
            Some("paused") => return Outcome::Refused,
            Some("pending") => return Outcome::Waiting,
            _ => {}
        }
    }
    Outcome::Ok
}

fn launch_error_node(data: &Value, outcome: Outcome) -> Option<&Value> {
    data["checkpoint"]["nodes"].as_array()?.iter().find(|node| {
        node["advance_error"].is_string()
            && node["attempts"].as_array().is_some_and(Vec::is_empty)
            && (matches!(outcome, Outcome::Failed | Outcome::Refused)
                || node["state"] == "failed"
                || (node["state"] == "blocked"
                    && matches!(
                        node["advance_error_code"].as_str(),
                        Some("launch_refused" | "launch_retry_exhausted")
                    )))
    })
}

fn cli_input_error(detail: &str) -> Box<provenant_records::diagnostic::Diagnostic> {
    Box::new(
        provenant_records::diagnostic::Diagnostic::new("invalid_input")
            .detail(detail)
            .next("check the named input field and retry"),
    )
}

fn file_input_error(path: &Path) -> Box<provenant_records::diagnostic::Diagnostic> {
    file_error(path, "input could not be read")
}

fn file_error(path: &Path, reason: &str) -> Box<provenant_records::diagnostic::Diagnostic> {
    let path = provenant_records::diagnostic::safe_path(
        path,
        provenant_records::diagnostic::Destination::Local,
    );
    cli_input_error(&format!(
        "--file {}: {reason}",
        path.as_deref().unwrap_or("input"),
    ))
}

/// `GRAPH STATE model=MODEL worktree=WORKTREE`, naming the handle that
/// `status` and `output` accept.
fn run_headline(
    checkpoint: &Value,
    state: &str,
    document: Option<&Value>,
    input: &Value,
) -> String {
    let graph = checkpoint["graph_id"].as_str().unwrap_or_default();
    let nodes = checkpoint["nodes"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    let state = match nodes {
        [node] => node["state"].as_str().unwrap_or(state),
        _ => state,
    };
    let route_text = |route: &Value| {
        let adapter = route["adapter"].as_str();
        match route["model"].as_str() {
            Some(model) if model.contains('/') => Some(model.to_owned()),
            Some(model) => {
                Some(adapter.map_or(model.to_owned(), |adapter| format!("{adapter}/{model}")))
            }
            None => route["name"]
                .as_str()
                .or_else(|| route["alias"].as_str())
                .or(adapter)
                .map(str::to_owned),
        }
    };
    let document_nodes = document
        .and_then(|document| document["nodes"].as_array())
        .map_or(&[][..], Vec::as_slice);
    let model = nodes
        .iter()
        .find_map(|node| {
            node["attempts"]
                .as_array()?
                .last()
                .and_then(|attempt| route_text(&attempt["route"]))
        })
        .or_else(|| {
            document_nodes.iter().find_map(|node| {
                route_text(&node["route"]).or_else(|| route_text(&node["routes"][0]))
            })
        })
        .or_else(|| input["model"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "-".into());
    let new_worktree = |branch: &Value| branch.as_str().map(|branch| format!("new:{branch}"));
    let worktree = document_nodes
        .iter()
        .find_map(|node| {
            node["worktree"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| new_worktree(&node["attempt"]["new_worktree"]["branch"]))
        })
        .or_else(|| input["worktree"].as_str().map(str::to_owned))
        .or_else(|| new_worktree(&input["branch"]))
        .unwrap_or_else(|| "-".into());
    format!("{graph} {state} model={model} worktree={worktree}")
}

/// The full `GRAPH/NODE/ATTEMPT` ID of each node's newest launched attempt.
fn lane_ids(checkpoint: &Value) -> Vec<String> {
    let graph = checkpoint["graph_id"].as_str().unwrap_or_default();
    checkpoint["nodes"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(|node| {
            let attempt = node["attempts"]
                .as_array()?
                .iter()
                .rev()
                .find_map(|attempt| attempt["attempt_ref"].as_str())?;
            Some(format!(
                "{graph}/{}/{}",
                node["id"].as_str()?,
                attempt.rsplit('/').next()?
            ))
        })
        .collect()
}

/// One line per launched lane, within the receipt's row budget.
fn lane_lines(checkpoint: &Value) -> String {
    let ids = lane_ids(checkpoint);
    let mut text: String = ids
        .iter()
        .take(provenant_cli::limits::SUMMARY_ROWS)
        .map(|id| format!("lane: {id}\n"))
        .collect();
    if ids.len() > provenant_cli::limits::SUMMARY_ROWS {
        text += &format!(
            "{} more lanes in --json\n",
            ids.len() - provenant_cli::limits::SUMMARY_ROWS
        );
    }
    for node in checkpoint["nodes"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
    {
        if node["advance_error_code"] == "preparation_refused"
            && let Some(message) = node["advance_error"].as_str()
        {
            text += &format!(
                "refused {}: {message}\n",
                node["id"].as_str().unwrap_or_default()
            );
        }
    }
    text
}

/// The compact `run` result: the headline, unless it was already announced,
/// then the one next command.
fn run_human(data: &Value, input: &Value, announced: bool) -> String {
    let graph = data["checkpoint"]["graph_id"].as_str().unwrap_or_default();
    let next = match data["status"].as_str() {
        Some("complete" | "cancelled") => format!("provenant output {graph}"),
        Some("paused") => format!("provenant status {graph}"),
        _ => format!("provenant status {graph} --wait"),
    };
    let status = data["status"].as_str().unwrap_or("pending");
    let document = input.get("document");
    let lanes = lane_lines(&data["checkpoint"]);
    if announced {
        format!("{lanes}next: {next}\n")
    } else {
        let headline = run_headline(&data["checkpoint"], status, document, input);
        format!("{headline}\n{lanes}next: {next}\n")
    }
}

fn invoke(
    command: &str,
    input: Result<Value, Box<provenant_records::diagnostic::Diagnostic>>,
    globals: &Globals,
    workers: &'static dyn WorkerObserver,
) -> u8 {
    let input = match input {
        Ok(input) => input,
        Err(projection) => {
            let mut result = CommandResult::new(command, Outcome::Invalid, Value::Null);
            result.error = Some(provenant_records::CommandError {
                diagnostic: Some(projection.as_ref().into()),
                code: projection.code.clone(),
                message: projection
                    .detail
                    .clone()
                    .unwrap_or_else(|| "graph input could not be processed".into()),
                cause: None,
                repair: projection.next,
            });
            return crate::emit(&result, globals, "", false);
        }
    };
    let announced = std::cell::Cell::new(false);
    let result = (|| -> Result<Value, Box<dyn std::error::Error>> {
        if command == "run" && std::env::var_os(provenant_run::PARENT_ATTEMPT_ENV).is_some() {
            let factory = fabric::read_factory(globals, workers)?;
            let wait_seconds = input["wait_seconds"].as_u64().unwrap_or(55).max(55);
            let budget = WaitBudget::cli(factory.clock.mono_now(), wait_seconds)?;
            return Ok(factory.relay_document(&input, budget, &CancelToken::new())?);
        }
        let factory =
            if input["explain"] == true || matches!(command, "templates" | "status" | "stats") {
                fabric::read_factory(globals, workers)?
            } else {
                fabric::factory(globals, workers)?
            };
        let wait_seconds = input["wait_seconds"].as_u64().unwrap_or(55);
        let budget = WaitBudget::cli(factory.clock.mono_now(), wait_seconds)?;
        let local_branch = (command == "run" && input["host"].is_null())
            .then(|| input["branch"].as_str().map(str::to_owned))
            .flatten();
        // The compact view names the lane as soon as its checkpoint is saved.
        let announce =
            |document: &provenant_fabric_runtime::graph::GraphDocument,
             checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint| {
                let document = serde_json::to_value(document).unwrap_or_default();
                let checkpoint = serde_json::to_value(checkpoint).unwrap_or_default();
                let line = run_headline(&checkpoint, "pending", Some(&document), &input) + "\n";
                let mut stdout = std::io::stdout().lock();
                if stdout
                    .write_all(line.as_bytes())
                    .and_then(|()| stdout.flush())
                    .is_ok()
                {
                    announced.set(true);
                }
            };
        let announce = (command == "run" && !globals.json && !globals.full)
            .then_some(&announce as fabric::Announce<'_>);
        let mut result = factory.graph_document_announcing(
            input.clone(),
            budget,
            &CancelToken::new(),
            announce,
        )?;
        if let Some(branch) = local_branch.as_deref()
            && result["checkpoint"]["nodes"]
                .as_array()
                .is_some_and(|nodes| {
                    nodes.iter().any(|node| {
                        node["state"] == "failed"
                            && node["advance_error_code"] == "writer_binding_unavailable"
                            && node["advance_owner_code"] != "writer_lock_busy"
                    })
                })
            && let Some(action) = local_branch_action(&factory, branch)
        {
            result["local_worktree_remedy"] = json!(action);
        }
        factory.name_refusal_causes(&mut result);
        if command == "run" && result["checkpoint"]["graph_id"].is_string() {
            result["lane_ids"] = json!(lane_ids(&result["checkpoint"]));
        }
        Ok(result)
    })();
    match result {
        Ok(data) => {
            let human = if command == "run"
                && !globals.full
                && data["checkpoint"]["graph_id"].is_string()
            {
                run_human(&data, &input, announced.get())
            } else {
                serde_json::to_string_pretty(&data).unwrap_or_default() + "\n"
            };
            let mut result = CommandResult::new(command, response_outcome(command, &data), data);
            if command == "run"
                && let Some(node) = launch_error_node(&result.data, result.status)
            {
                result.error = Some(provenant_records::CommandError {
                    code: node["advance_owner_code"]
                        .as_str()
                        .or_else(|| node["advance_error_code"].as_str())
                        .unwrap_or("launch_failed")
                        .into(),
                    message: node["advance_error"]
                        .as_str()
                        .unwrap_or("launch failed")
                        .into(),
                    diagnostic: None,
                    cause: None,
                    repair: None,
                });
            }
            crate::emit(
                &result,
                globals,
                &human,
                matches!(command, "templates" | "status" | "stats"),
            )
        }
        Err(error) => {
            let projection = error
                .downcast_ref::<provenant_fabric_runtime::coordination::facade::FacadeError>()
                .map(|error| error.diagnostic())
                .or_else(|| {
                    error
                        .downcast_ref::<provenant_cli::fabric::EdgeError>()
                        .and_then(provenant_cli::fabric::edge_diagnostic)
                })
                .unwrap_or_else(|| *cli_input_error("graph input could not be processed"));
            let outcome = error
                .downcast_ref::<provenant_fabric_runtime::coordination::facade::FacadeError>()
                .map_or(
                    Outcome::Invalid,
                    provenant_cli::fabric::coordination_outcome,
                );
            let mut result = CommandResult::new(command, outcome, Value::Null);
            result.error = Some(provenant_records::CommandError {
                diagnostic: Some((&projection).into()),
                code: projection.code.as_str().into(),
                message: projection
                    .detail
                    .clone()
                    .unwrap_or_else(|| "graph input could not be processed".into()),
                cause: None,
                repair: projection.next,
            });
            crate::emit(&result, globals, "", false)
        }
    }
}

/// Observe a local checkout only after graph admission has reported a writer
/// binding failure. This advisory reading never changes replay or custody.
pub(crate) fn local_branch_action(factory: &fabric::Factory, branch: &str) -> Option<String> {
    use provenant_confinement::worktrees;
    let reference = provenant_confinement::repo::GitRefName::branch(branch).ok()?;
    let cancel = CancelToken::new();
    let path = worktrees::with_context_using_workers(
        &factory.context.roots,
        &factory.home,
        &factory.cwd,
        &factory.clock,
        &cancel,
        factory.workers,
        |context| {
            let candidate = worktrees::list_for_gc_matching(context, &cancel, |_, branch| {
                branch == Some(reference.as_str())
            })?
            .into_iter()
            .find(|entry| {
                entry.is_unowned()
                    && entry.branch.as_deref() == Some(reference.as_str())
                    && entry.path.is_absolute()
            });
            let Some(entry) = candidate else {
                return Ok(None);
            };
            let mut tree = provenant_confinement::verify_worktree(context.repo, &entry.path)?;
            let _guard = worktrees::cache_writer_guard(context.project_state, tree.path())
                .map_err(worktrees::custody_refusal)?;
            worktrees::require_clean_worktree(context, &mut tree, &cancel)?;
            Ok(Some(entry.path))
        },
    )
    .ok()??;
    provenant_run::RunError::Writer(worktrees::WorktreeError::UnknownWriterAt { path })
        .diagnostic(provenant_records::diagnostic::Destination::Local)
        .next
}
#[allow(clippy::print_stderr)]
pub(crate) fn run(
    mut request: GraphRunArgs,
    globals: &Globals,
    workers: &'static dyn WorkerObserver,
) -> u8 {
    if let Some(value) = request.ambiguous_wait_brief().map(str::to_owned) {
        let message = format!("`--wait` has no value; use `--wait={value}`");
        let _ = writeln!(std::io::stderr().lock(), "warning: {message}");
        request.brief = None;
        if request.file.is_none() {
            return invoke("run", Err(cli_input_error(&message)), globals, workers);
        }
    }
    invoke("run", run_input(request), globals, workers)
}

/// A `--model` without an adapter names the one catalogued model that carries
/// it. `ADAPTER/ID` passes through unchanged for the facade to check.
fn model_selector(model: String) -> Result<String, Box<provenant_records::diagnostic::Diagnostic>> {
    if model.contains('/') {
        return Ok(model);
    }
    let catalogue = provenant_routing::Catalogue::bundled()
        .map_err(|_| cli_input_error("the model catalogue could not be loaded"))?;
    catalogue
        .resolve_bare_model(&model)
        .map(|id| id.as_str().to_owned())
        .map_err(|error| cli_input_error(&bare_model_detail(&model, &error)))
}

fn bare_model_detail(model: &str, error: &BareModelError) -> String {
    // Echo the name only when it is identifier-shaped, so a pasted secret is
    // never repeated.
    let shown = if !model.is_empty()
        && model.len() <= 64
        && model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        format!("`{model}`")
    } else {
        "the model".into()
    };
    match error {
        BareModelError::Ambiguous { candidates } => format!(
            "model {shown} is offered by several providers: {}; name one as provider/model",
            candidates.join(", ")
        ),
        BareModelError::Unknown => format!(
            "unknown model {shown}; no catalogued provider offers it, so name it in the provider/model form"
        ),
    }
}

fn run_input(
    request: GraphRunArgs,
) -> Result<Value, Box<provenant_records::diagnostic::Diagnostic>> {
    (|| {
        let model = request.model.map(model_selector).transpose()?;
        let mut input = json!({"op":"run","explain":request.explain,"detached":request.detached});
        for (field, value) in [
            ("host", request.host),
            ("model", model),
            ("effort", request.effort),
            ("account", request.account),
            (
                "operation_id",
                request.operation_id.map(|id| id.to_string()),
            ),
        ] {
            if let Some(value) = value {
                input[field] = json!(value);
            }
        }
        if let Some(mode) = request.mode {
            input["access"] = json!(mode.replace('-', "_"));
        }
        if let Some(path) = request.worktree {
            input["worktree"] = json!(path);
        }
        if let Some(branch) = request.branch {
            input["branch"] = json!(branch);
        }
        if let Some(base) = request.base {
            input["base"] = json!(base);
        }
        if !request.write_scope.is_empty() {
            input["write_scope"] = json!(request.write_scope);
        }
        if let Some(wait) = request.wait {
            input["wait_seconds"] = json!(wait);
        }
        if let Some(name) = request.template {
            input["use"] = json!(name);
        }
        let brief_selected = request.tier.is_some();
        if let Some(tier) = request.tier {
            input["tier"] = json!(tier);
        }
        if !request.over.is_empty() {
            input["over"] = if request.over.len() == 1 && request.over[0].starts_with('@') {
                json!(request.over[0])
            } else {
                json!(request.over)
            };
        }
        let mut parameters = json!({});
        for item in request.set {
            let (key, value) = item
                .split_once('=')
                .ok_or_else(|| cli_input_error("--set expects NAME=TOML_VALUE"))?;
            let parsed: Value =
                toml_edit::de::from_str(&format!("value = {value}")).map_err(|_| {
                    let safe = matches!(
                        key,
                        "attempt"
                            | "attempt.cloud"
                            | "attempt.placement"
                            | "attempt.host"
                            | "attempt.account"
                            | "attempt.confidential"
                            | "attempt.provider_timeout"
                            | "attempt.capabilities"
                            | "attempt.build_heavy"
                            | "attempt.write_roots"
                    );
                    if safe {
                        cli_input_error(&format!("--set `{key}`: value is not valid TOML"))
                    } else {
                        cli_input_error("--set value is not valid TOML")
                    }
                })?;
            if key.is_empty() {
                return Err(cli_input_error("--set parameter name is empty"));
            }
            validate_set_value_path(key, &parsed["value"])?;
            parameters[key] = parsed["value"].clone();
        }
        input["set"] = parameters;
        if let Some(brief) = request.brief {
            input["brief"] = json!(brief);
        }
        if let Some(path) = request.file {
            let text = std::fs::read_to_string(&path).map_err(|_| file_input_error(&path))?;
            if text.trim().is_empty() {
                return Err(file_error(&path, "brief is empty"));
            }
            match decode(&text) {
                Ok(value)
                    if value.as_object().is_some_and(|object| {
                        // A route override must not turn an explicit graph
                        // into the prompt of a newly generated task.
                        object.contains_key("nodes")
                            || object.contains_key("graph_id")
                            || (!brief_selected && !object.is_empty())
                    }) =>
                {
                    input["document"] = value;
                }
                _ => input["brief"] = json!(text),
            }
        }
        Ok(input)
    })()
}

fn validate_set_path(key: &str) -> Result<(), Box<provenant_records::diagnostic::Diagnostic>> {
    let Some(path) = key.strip_prefix("attempt.") else {
        return Ok(());
    };
    match path {
        "mode" => Err(cli_input_error("use --mode for attempt access mode")),
        "worktree" => Err(cli_input_error("use --worktree for the writer worktree")),
        "branch" | "base" | "new_worktree" => Err(cli_input_error(
            "use --branch B --base REF for a new writer worktree",
        )),
        "host" | "account" | "confidential" | "provider_timeout" | "placement" | "cloud"
        | "capabilities" | "capability" | "build_heavy" | "build_weight" | "write_roots"
        | "write_root" | "local_cargo" | "preset" | "isolation" | "supervision" | "receipts" => {
            Ok(())
        }
        _ if path.starts_with("mode.") => {
            Err(cli_input_error("use --mode for attempt access mode"))
        }
        _ if path.starts_with("worktree.") => {
            Err(cli_input_error("use --worktree for the writer worktree"))
        }
        _ => Err(cli_input_error(
            "--set path is not applied by run; use --set NAME=TOML_VALUE for a declared template parameter or --mode, --worktree, --branch or --base for launch controls",
        )),
    }
}

fn validate_set_value_path(
    key: &str,
    value: &Value,
) -> Result<(), Box<provenant_records::diagnostic::Diagnostic>> {
    validate_set_path(key)?;
    if key == "attempt"
        && let Some(fields) = value.as_object()
    {
        for (field, value) in fields {
            validate_set_value_path(&format!("attempt.{field}"), value)?;
        }
    }
    Ok(())
}
pub(crate) fn edit(request: Edit, globals: &Globals, workers: &'static dyn WorkerObserver) -> u8 {
    invoke(
        "edit",
        document(&request.file).map(|patch| json!({"op":"edit","id":request.id,"patch":patch})),
        globals,
        workers,
    )
}
pub(crate) fn templates(
    request: Templates,
    globals: &Globals,
    workers: &'static dyn WorkerObserver,
) -> u8 {
    let input = (|| {
        let mut input = json!({"op":"templates"});
        if let Some(name) = request.name {
            input["name"] = json!(name);
        }
        if let Some(path) = request.file {
            let value = document(&path)?;
            input["templates"] = value.get("templates").unwrap_or(&value).clone();
        }
        Ok(input)
    })();
    invoke("templates", input, globals, workers)
}
pub(crate) fn stats(
    command: &str,
    id: Option<String>,
    by: String,
    globals: &Globals,
    workers: &'static dyn WorkerObserver,
) -> u8 {
    invoke(
        command,
        Ok(json!({"op":"status","id":id,"stats":true,"by":by})),
        globals,
        workers,
    )
}

#[cfg(test)]
#[allow(
    unused_imports,
    reason = "the custom worktree driver includes this module without libtest"
)]
mod tests {
    use super::*;

    #[allow(
        dead_code,
        reason = "the custom worktree driver omits libtest functions"
    )]
    fn run_request() -> GraphRunArgs {
        GraphRunArgs {
            host: None,
            model: None,
            effort: None,
            account: None,
            operation_id: None,
            brief: None,
            template: None,
            tier: None,
            mode: None,
            worktree: None,
            branch: None,
            base: None,
            write_scope: Vec::new(),
            over: Vec::new(),
            set: Vec::new(),
            file: None,
            detached: false,
            explain: false,
            wait: None,
            bare_wait_value: None,
        }
    }

    #[test]
    fn malformed_set_and_missing_file_preserve_input_detail()
    -> Result<(), Box<dyn std::error::Error>> {
        for (setting, detail) in [
            ("broken", "--set expects NAME=TOML_VALUE"),
            ("=1", "--set parameter name is empty"),
            ("count=", "TOML"),
        ] {
            let mut request = run_request();
            request.set.push(setting.into());
            let error = run_input(request).err().ok_or("invalid set was accepted")?;
            let diagnostic = error;
            assert_eq!(diagnostic.code, "invalid_input");
            assert!(
                diagnostic
                    .detail
                    .as_deref()
                    .is_some_and(|text| text.contains(detail))
            );
        }
        let mut request = run_request();
        request.set.push("x='PROMPT_SENTINEL".into());
        let diagnostic = run_input(request).unwrap_err();
        assert!(!serde_json::to_string(&diagnostic)?.contains("PROMPT_SENTINEL"));
        let directory = tempfile::tempdir()?;
        let mut request = run_request();
        request.file = Some(directory.path().join("missing.json"));
        let error = run_input(request)
            .err()
            .ok_or("missing file was accepted")?;
        let diagnostic = error;
        assert_eq!(diagnostic.code, "invalid_input");
        assert!(
            diagnostic
                .detail
                .as_deref()
                .is_some_and(|text| text.contains("--file") && text.contains("missing.json"))
        );
        Ok(())
    }

    #[test]
    fn run_flags_and_attempt_settings_reach_the_graph_request() {
        let provenant_cli::ParseOutcome::Invocation(invocation) = provenant_cli::parse(
            [
                "run",
                "Inspect",
                "--host",
                "workshop",
                "--model",
                "codex/gpt-6-luna",
                "--effort",
                "high",
                "--account",
                "work",
                "--operation-id",
                "host-run",
                "--set",
                "attempt.build_heavy=true",
                "--set",
                "attempt.confidential=true",
                "--set",
                "attempt.provider_timeout=42",
                "--set",
                "attempt.host='other'",
            ]
            .map(std::ffi::OsString::from),
        ) else {
            panic!("run should parse");
        };
        let provenant_cli::Command::Run(request) = invocation.command else {
            panic!("run command");
        };
        let input = run_input(*request).unwrap();
        assert_eq!(input["host"], "workshop");
        assert_eq!(input["model"], "codex/gpt-6-luna");
        assert_eq!(input["effort"], "high");
        assert_eq!(input["account"], "work");
        assert_eq!(input["operation_id"], "host-run");
        assert_eq!(input["set"]["attempt.build_heavy"], true);
        assert_eq!(input["set"]["attempt.confidential"], true);
        assert_eq!(input["set"]["attempt.provider_timeout"], 42);
        assert_eq!(input["set"]["attempt.host"], "other");
    }

    #[test]
    fn bare_model_resolves_to_its_provider_and_refuses_otherwise() {
        let with_model = |model: &str| {
            let mut request = run_request();
            request.model = Some(model.into());
            run_input(request)
        };
        // One provider offers it: the bare name becomes ADAPTER/ID.
        let input = with_model("claude-sonnet-5-5").unwrap();
        assert_eq!(input["model"], "claude/claude-sonnet-5-5");
        // The provider-qualified form is passed through unchanged.
        let input = with_model("codex/gpt-6.1-terra").unwrap();
        assert_eq!(input["model"], "codex/gpt-6.1-terra");
        // No provider offers it: refused as invalid input, naming the form to use.
        let refusal = with_model("no-such-model-9").unwrap_err();
        assert_eq!(refusal.code, "invalid_input");
        let detail = refusal.detail.unwrap_or_default();
        assert!(
            detail.contains("unknown model `no-such-model-9`"),
            "{detail}"
        );
        assert!(detail.contains("provider/model form"), "{detail}");
    }

    #[test]
    fn ambiguous_bare_model_names_each_provider_form() {
        let detail = bare_model_detail(
            "gpt-6.1-terra",
            &BareModelError::Ambiguous {
                candidates: vec!["codex/gpt-6.1-terra".into(), "cursor/gpt-6.1-terra".into()],
            },
        );
        assert_eq!(
            detail,
            "model `gpt-6.1-terra` is offered by several providers: codex/gpt-6.1-terra, cursor/gpt-6.1-terra; name one as provider/model"
        );
        // A name that is not identifier-shaped is never echoed.
        let secret = bare_model_detail("sk-x y", &BareModelError::Unknown);
        assert!(!secret.contains("sk-x y"), "{secret}");
    }

    #[test]
    fn attempt_access_paths_refuse_and_name_the_launch_flag() {
        for (setting, flag) in [
            ("attempt.mode='worktree-write'", "--mode"),
            ("attempt.worktree='W'", "--worktree"),
            ("attempt.branch='B'", "--branch B --base REF"),
            ("attempt={mode='worktree-write'}", "--mode"),
            ("attempt.unapplied=true", "declared template parameter"),
        ] {
            let mut request = run_request();
            request.set.push(setting.into());
            let error = run_input(request).expect_err("unsupported launch path must refuse");
            assert!(
                error
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains(flag)),
                "{setting}: {error:?}"
            );
        }
    }

    #[test]
    fn attempt_cloud_alias_passes_in_both_set_forms() {
        let cloud = "{target='t',provider='p',environment='e',repository='r',branch='b',model='m',deadline_seconds=60}";
        for setting in [
            format!("attempt.cloud={cloud}"),
            format!("attempt={{cloud={cloud}}}"),
        ] {
            let mut request = run_request();
            request.set.push(setting);
            let input = run_input(request).expect("cloud placement is applied");
            let value = input["set"]
                .get("attempt.cloud")
                .unwrap_or(&input["set"]["attempt"]["cloud"]);
            let attempt: provenant_fabric_runtime::graph::AttemptSpec =
                serde_json::from_value(serde_json::json!({"cloud": value})).unwrap();
            assert_eq!(attempt.placement.unwrap().target, "t");
        }
    }

    #[test]
    fn pending_graph_projects_failed_pre_attempt_node() {
        let data = json!({"status":"pending", "checkpoint":{"nodes":[
            {"id":"retry", "state":"blocked", "attempts":[], "advance_error":"retrying"},
            {"id":"writer", "state":"failed", "attempts":[], "advance_error":"binding failed"},
            {"id":"other", "state":"running", "attempts":[{}]}
        ]}});
        assert_eq!(response_outcome("run", &data), Outcome::Waiting);
        assert_eq!(
            launch_error_node(&data, Outcome::Waiting).map(|node| &node["id"]),
            Some(&json!("writer"))
        );
        let mut timed_out = data.clone();
        timed_out["timed_out"] = json!(true);
        assert_eq!(response_outcome("run", &timed_out), Outcome::Waiting);
        assert_eq!(
            launch_error_node(&timed_out, Outcome::Waiting).map(|node| &node["id"]),
            Some(&json!("writer"))
        );
    }

    #[test]
    fn pending_graph_projects_definitive_blocked_launch_errors() {
        for code in ["launch_refused", "launch_retry_exhausted"] {
            let data = json!({"status":"pending", "timed_out":true, "checkpoint":{"nodes":[
                {"id":"slow", "state":"running", "attempts":[{}]},
                {"id":"writer", "state":"blocked", "attempts":[],
                 "advance_error":"bounded launch refused", "advance_error_code":code,
                 "advance_owner_code":"writer_lock_busy"}
            ]}});
            assert_eq!(response_outcome("run", &data), Outcome::Waiting);
            let node = launch_error_node(&data, Outcome::Waiting).unwrap();
            assert_eq!(node["id"], "writer");
            assert_eq!(node["advance_owner_code"], "writer_lock_busy");
            let mut retrying = data.clone();
            retrying["checkpoint"]["nodes"][1]["advance_error_code"] = json!("retrying");
            assert!(launch_error_node(&retrying, Outcome::Waiting).is_none());
            let mut launched = data.clone();
            launched["checkpoint"]["nodes"][1]["attempts"] = json!([{}]);
            assert!(launch_error_node(&launched, Outcome::Waiting).is_none());
        }
    }

    #[test]
    fn run_result_is_a_headline_and_one_next_command() {
        let data = json!({"status":"pending", "timed_out":false, "checkpoint":{"graph_id":"g-1", "nodes":[
            {"id":"task", "state":"running", "attempts":[
                {"route":{"adapter":"claude", "model":"claude-opus-5-5"}, "attempt_ref":"a/b"}
            ]}
        ]}});
        let input = json!({"op":"run", "worktree":"/repo/.worktrees/lane"});
        let human = run_human(&data, &input, false);
        assert_eq!(
            human,
            "g-1 running model=claude/claude-opus-5-5 worktree=/repo/.worktrees/lane\n\
             lane: g-1/task/b\n\
             next: provenant status g-1 --wait\n"
        );
        assert!(human.lines().count() <= 3);
        assert_eq!(
            run_human(&data, &input, true),
            "lane: g-1/task/b\nnext: provenant status g-1 --wait\n"
        );
        assert_eq!(lane_ids(&data["checkpoint"]), ["g-1/task/b"]);
        let mut complete = data.clone();
        complete["status"] = json!("complete");
        assert!(run_human(&complete, &input, true).ends_with("next: provenant output g-1\n"));
    }

    #[test]
    fn receipt_lists_refused_cause_and_bounds_lane_lines() {
        let nodes: Vec<_> = (0..15)
            .map(|n| json!({"id":format!("n{n}"), "state":"running", "attempts":[{"attempt_ref":format!("h/a{n}")}]}))
            .collect();
        let text = lane_lines(&json!({"graph_id":"g", "nodes":nodes}));
        assert_eq!(text.lines().count(), 13, "{text}");
        assert!(text.contains("lane: g/n0/a0\n"));
        let refused = json!({"graph_id":"g", "nodes":[
            {"id":"r", "state":"failed", "advance_error_code":"preparation_refused",
             "advance_error":"refused. Refused: codex/gpt: allowance spent", "attempts":[]}
        ]});
        assert!(lane_lines(&refused).contains("allowance spent"));
    }

    #[test]
    fn announced_headline_reads_the_document_before_any_attempt() {
        let checkpoint = json!({"graph_id":"g-2", "nodes":[
            {"id":"one", "state":"pending", "attempts":[]},
            {"id":"two", "state":"pending", "attempts":[]}
        ]});
        let document = json!({"graph_id":"g-2", "nodes":[
            {"id":"one", "route":{"alias":"senior"},
             "attempt":{"new_worktree":{"branch":"batch/work", "base":"main"}}},
            {"id":"two", "routes":[{"adapter":"codex", "model":"gpt"}]}
        ]});
        assert_eq!(
            run_headline(&checkpoint, "pending", Some(&document), &json!({})),
            "g-2 pending model=senior worktree=new:batch/work"
        );
        assert_eq!(
            run_headline(&checkpoint, "pending", None, &json!({})),
            "g-2 pending model=- worktree=-"
        );
    }

    #[test]
    fn failed_attempt_does_not_project_launch_error() {
        let data = json!({"checkpoint":{"nodes":[
            {"id":"task", "state":"failed", "attempts":[{}], "advance_error":"fallback_route_pinned"}
        ]}});
        assert!(launch_error_node(&data, Outcome::Failed).is_none());
    }

    #[test]
    fn run_reports_the_graph_outcome_and_status_reads_remain_successful() {
        for (status, outcome, expected) in [
            ("complete", "failed", Outcome::Failed),
            ("complete", "succeeded", Outcome::Ok),
            ("complete", "partial", Outcome::Ok),
            ("cancelled", "failed", Outcome::Cancelled),
            ("paused", "failed", Outcome::Refused),
            ("pending", "partial", Outcome::Waiting),
        ] {
            let data = json!({"status":status,"outcome":outcome});
            assert_eq!(response_outcome("run", &data), expected);
            assert_eq!(response_outcome("status", &data), Outcome::Ok);
        }
        assert_eq!(
            response_outcome("run", &json!({"document":{}})),
            Outcome::Ok
        );
        assert_eq!(
            response_outcome("run", &json!({"timed_out":true})),
            Outcome::Waiting
        );
    }
}
