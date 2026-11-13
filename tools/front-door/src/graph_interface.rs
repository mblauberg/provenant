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
    if command == "run" && data["named"]["replayed"] != true {
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
    file_error("--file", path, "input could not be read")
}

fn file_error(
    flag: &str,
    path: &Path,
    reason: &str,
) -> Box<provenant_records::diagnostic::Diagnostic> {
    let path = provenant_records::diagnostic::safe_path(
        path,
        provenant_records::diagnostic::Destination::Local,
    );
    cli_input_error(&format!(
        "{flag} {}: {reason}",
        path.as_deref().unwrap_or("input"),
    ))
}

/// `NAME STATE MODEL WORKTREE`, naming the handle that
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
    format!("{graph} {state} {model} {worktree}")
}

/// One line per node the owner refused before admission, within the receipt's
/// row budget. A run that launched cleanly prints none.
fn refusal_lines(checkpoint: &Value) -> String {
    let mut text = String::new();
    let refused: Vec<String> = checkpoint["nodes"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter(|node| node["advance_error_code"] == "preparation_refused")
        .filter_map(|node| {
            let message = node["advance_error"].as_str()?;
            let line = format!(
                "refused {}: {}",
                node["id"].as_str().unwrap_or_default(),
                message.split_whitespace().collect::<Vec<_>>().join(" ")
            );
            Some(provenant_cli::bounded_text(
                &line,
                provenant_cli::limits::SUMMARY_ROW_BYTES,
            ))
        })
        .collect();
    for line in refused.iter().take(provenant_cli::limits::SUMMARY_ROWS) {
        text += &format!("{line}\n");
    }
    if refused.len() > provenant_cli::limits::SUMMARY_ROWS {
        text += &format!(
            "{} more refused nodes in --json\n",
            refused.len() - provenant_cli::limits::SUMMARY_ROWS
        );
    }
    text
}

/// The compact `run` result: the headline, unless it was already announced,
/// then the one next command. A refusal before admission adds its own line.
fn run_human(data: &Value, input: &Value, announced: bool) -> String {
    let graph = data["checkpoint"]["graph_id"].as_str().unwrap_or_default();
    let next = match data["status"].as_str() {
        Some("complete" | "cancelled") => format!("provenant output {graph}"),
        Some("paused") => format!("provenant status {graph}"),
        _ => format!("provenant wait {graph}"),
    };
    let status = data["status"].as_str().unwrap_or("pending");
    let document = input.get("document");
    let mut lanes = String::new();
    if let (Some(taken), true) = (
        data["named"]["taken"].as_str(),
        data["named"]["graph"].is_string(),
    ) {
        lanes += &format!("{taken} exists with different input; started {graph}\n");
    }
    lanes += &refusal_lines(&data["checkpoint"]);
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
        let factory = if input["explain"] == true || command == "templates" {
            fabric::read_factory(globals, workers)?
        } else {
            fabric::factory(globals, workers)?
        };
        // `run` takes no wait: its launch, including binding a writer, has the
        // configured launch allowance.
        let wait_seconds = input["wait_seconds"]
            .as_u64()
            .unwrap_or(factory.context.config.graph.writer_launch_seconds);
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
            // A replay reports the graph its input already started; its failure
            // was reported when it happened.
            let replayed = data["named"]["replayed"] == true;
            let mut result = CommandResult::new(command, response_outcome(command, &data), data);
            if command == "run"
                && !replayed
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
            crate::emit(&result, globals, &human, command == "templates")
        }
        Err(error) => {
            let facade =
                error.downcast_ref::<provenant_fabric_runtime::coordination::facade::FacadeError>();
            let projection = facade
                .map(|error| error.diagnostic())
                .or_else(|| {
                    error
                        .downcast_ref::<provenant_cli::fabric::EdgeError>()
                        .and_then(provenant_cli::fabric::edge_diagnostic)
                })
                .unwrap_or_else(|| *cli_input_error("graph input could not be processed"));
            let outcome = facade.map_or(
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
                cause: facade.and_then(|error| error.cause()).map(str::to_owned),
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
    request: GraphRunArgs,
    globals: &Globals,
    workers: &'static dyn WorkerObserver,
) -> u8 {
    if request.over.len() > 5 {
        let _ = writeln!(
            std::io::stderr().lock(),
            "warning: --over creates {} nodes; check comma-separated items",
            request.over.len()
        );
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

/// The brief for `--brief-file`: a file, or standard input given `-`.
fn read_brief(path: &Path) -> Result<String, Box<provenant_records::diagnostic::Diagnostic>> {
    use std::io::Read;
    let mut text = String::new();
    let read = if path == Path::new("-") {
        std::io::stdin().lock().read_to_string(&mut text)
    } else {
        std::fs::File::open(path).and_then(|mut file| file.read_to_string(&mut text))
    };
    if read.is_err() {
        return Err(file_error(
            "--brief-file",
            path,
            "brief could not be read as text",
        ));
    }
    if text.trim().is_empty() {
        return Err(file_error("--brief-file", path, "brief is empty"));
    }
    Ok(text)
}

fn run_input(
    request: GraphRunArgs,
) -> Result<Value, Box<provenant_records::diagnostic::Diagnostic>> {
    (|| {
        let sandbox = request.sandbox_override();
        let model = request.model.map(model_selector).transpose()?;
        let mut input = json!({"op":"run","explain":request.explain,"detached":request.detached});
        for (field, value) in [
            ("host", request.host),
            ("model", model),
            ("effort", request.effort),
            ("account", request.account),
            ("name", request.name),
            ("template", request.template),
            ("route", request.route),
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
        if !request.over.is_empty() {
            input["over"] = if request.over.len() == 1 && request.over[0].starts_with('@') {
                json!(request.over[0])
            } else {
                json!(request.over)
            };
        }
        // Values travel as the text the caller wrote. The launch owner holds the
        // template library, so it types each one by the declaration it meets.
        let mut parameters = json!({});
        for item in request.set {
            let (key, value) = item
                .split_once('=')
                .ok_or_else(|| cli_input_error("--set expects NAME=VALUE"))?;
            if key.is_empty() {
                return Err(cli_input_error("--set parameter name is empty"));
            }
            validate_set_path(key)?;
            if key == "attempt" {
                let parsed: Value = toml_edit::de::from_str(&format!("value = {value}"))
                    .map_err(|_| cli_input_error("--set `attempt`: value is not valid TOML"))?;
                validate_set_value_path(key, &parsed["value"])?;
                parameters[key] = parsed["value"].clone();
            } else {
                parameters[key] = json!(value);
            }
        }
        if let Some(sandbox) = sandbox {
            parameters["attempt.sandbox"] = json!(sandbox);
        }
        input["set"] = parameters;
        if let Some(brief) = request.brief {
            input["brief"] = json!(brief);
        }
        if let Some(path) = request.brief_file {
            input["brief"] = json!(read_brief(&path)?);
        }
        if let Some(path) = request.file {
            let text = std::fs::read_to_string(&path).map_err(|_| file_input_error(&path))?;
            if text.trim().is_empty() {
                return Err(file_error("--file", &path, "document is empty"));
            }
            match decode(&text) {
                Ok(value) if value.as_object().is_some_and(|object| !object.is_empty()) => {
                    input["document"] = value;
                }
                _ => {
                    return Err(file_error(
                        "--file",
                        &path,
                        "not a graph document; use --brief-file for a brief",
                    ));
                }
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
        "placement" | "cloud" => Err(cli_input_error(
            provenant_fabric_runtime::graph::CLOUD_PLACEMENT_REMOVED,
        )),
        "host" | "account" | "confidential" | "provider_timeout" | "capabilities"
        | "capability" | "build_heavy" | "build_weight" | "write_roots" | "write_root"
        | "local_cargo" | "preset" | "isolation" | "supervision" | "receipts" | "sandbox"
        | "token_budget" | "on_budget" | "budget_grace" => Ok(()),
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
        document(&request.file).map(|patch| json!({"op":"edit","id":request.name,"patch":patch})),
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
            name: None,
            brief: None,
            brief_file: None,
            template: None,
            route: None,
            mode: None,
            worktree: None,
            branch: None,
            base: None,
            write_scope: Vec::new(),
            over: Vec::new(),
            set: Vec::new(),
            file: None,
            detached: false,
            sandbox: false,
            no_sandbox: false,
            explain: false,
        }
    }

    #[test]
    fn malformed_set_and_missing_file_preserve_input_detail()
    -> Result<(), Box<dyn std::error::Error>> {
        for (setting, detail) in [
            ("broken", "--set expects NAME=VALUE"),
            ("=1", "--set parameter name is empty"),
            ("attempt=", "TOML"),
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
        request.set.push("attempt='PROMPT_SENTINEL".into());
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
        // Values travel as written; the launch owner types them by declaration.
        assert!(input.get("operation_id").is_none());
        for (key, text) in [
            ("attempt.build_heavy", "true"),
            ("attempt.confidential", "true"),
            ("attempt.provider_timeout", "42"),
            ("attempt.host", "'other'"),
        ] {
            assert_eq!(input["set"][key], text);
        }
    }

    #[test]
    fn sandbox_flags_set_the_attempt_sandbox_and_conflict() {
        let parse = |flags: &[&str]| {
            provenant_cli::parse(
                ["run", "Inspect"]
                    .iter()
                    .chain(flags)
                    .map(std::ffi::OsString::from),
            )
        };
        for (flag, value) in [("--sandbox", "on"), ("--no-sandbox", "off")] {
            let provenant_cli::ParseOutcome::Invocation(invocation) = parse(&[flag]) else {
                panic!("{flag} should parse");
            };
            let provenant_cli::Command::Run(request) = invocation.command else {
                panic!("run command");
            };
            let input = run_input(*request).unwrap();
            assert_eq!(input["set"]["attempt.sandbox"], value);
        }
        assert!(!matches!(
            parse(&["--sandbox", "--no-sandbox"]),
            provenant_cli::ParseOutcome::Invocation(_)
        ));
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
    fn attempt_cloud_placement_is_refused_in_both_set_forms() {
        let cloud = "{target='t',provider='p'}";
        for setting in [
            format!("attempt.cloud={cloud}"),
            format!("attempt={{cloud={cloud}}}"),
            format!("attempt.placement={cloud}"),
        ] {
            let mut request = run_request();
            request.set.push(setting.clone());
            let error = run_input(request).expect_err("cloud placement must refuse");
            assert!(
                error
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("cloud launch")),
                "{setting}: {error:?}"
            );
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
    fn run_receipt_is_bounded_and_ends_in_one_next_command() {
        let nodes: Vec<_> = (0..40)
            .map(|n| json!({"id":format!("n{n}"), "state":"failed", "advance_error_code":"preparation_refused",
                "advance_error":"x".repeat(1000), "attempts":[]}))
            .collect();
        for (status, command) in [
            ("pending", "provenant wait review-1"),
            ("complete", "provenant output review-1"),
            ("paused", "provenant status review-1"),
        ] {
            let data =
                json!({"status":status, "checkpoint":{"graph_id":"review-1", "nodes":nodes}});
            for announced in [false, true] {
                let human = run_human(&data, &json!({"op":"run"}), announced);
                assert!(
                    human.starts_with(if announced { "refused" } else { "review-1" }),
                    "{human}"
                );
                assert!(human.ends_with(&format!("next: {command}\n")), "{human}");
                assert!(
                    human
                        .lines()
                        .filter(|line| line.starts_with("next:"))
                        .count()
                        == 1
                );
                assert!(
                    human
                        .lines()
                        .all(|line| line.len() <= provenant_cli::limits::SUMMARY_ROW_BYTES)
                );
                assert!(human.lines().count() <= provenant_cli::limits::SUMMARY_ROWS + 4);
            }
        }
    }

    #[test]
    fn a_suffixed_name_is_announced_and_a_replay_is_not_a_failure() {
        let data = json!({"status":"pending", "named":{"requested":"fix", "graph":"fix-2", "taken":"fix"},
            "checkpoint":{"graph_id":"fix-2", "nodes":[]}});
        let human = run_human(&data, &json!({}), false);
        assert!(
            human.contains("fix exists with different input; started fix-2"),
            "{human}"
        );
        let replay = json!({"status":"complete", "outcome":"failed", "named":{"graph":"fix", "replayed":true}});
        assert_eq!(response_outcome("run", &replay), Outcome::Ok);
        let fresh = json!({"status":"complete", "outcome":"failed"});
        assert_eq!(response_outcome("run", &fresh), Outcome::Failed);
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
            "g-2 pending senior new:batch/work"
        );
        assert_eq!(
            run_headline(&checkpoint, "pending", None, &json!({})),
            "g-2 pending - -"
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
