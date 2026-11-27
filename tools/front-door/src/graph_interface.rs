//! CLI documents for the shared graph service.
use crate::fabric;
use provenant_cli::brief::{BriefSource, read_brief, select_brief_source};
use provenant_cli::{Edit, Globals, GraphRunArgs, Templates};
use provenant_confinement::worktrees::WorkerObserver;
use provenant_core::{CancelToken, Clock, WaitBudget};
use provenant_fabric_runtime::graph::root_cause::is_upstream_blocked;
use provenant_records::{
    CommandResult, Outcome,
    lane_contract::{ContextInputs, LaunchMode},
    warning::{Warning, warn},
};
use provenant_routing::BareModelError;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};

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
        // An inline `run` that outlives `--timeout` reports 124 with
        // `still_running`: the lane keeps its lease and the caller waits.
        return if command == "run" {
            Outcome::Timeout
        } else {
            Outcome::Waiting
        };
    }
    if command == "run" && data["named"]["replayed"] != true {
        match data["status"].as_str() {
            Some("complete") if data["outcome"] == "failed" => return Outcome::Failed,
            Some("cancelled") => return Outcome::Cancelled,
            Some("paused" | "refused") => return Outcome::Refused,
            Some("pending") => return Outcome::Waiting,
            _ => {}
        }
    }
    Outcome::Ok
}

fn launch_error_node(data: &Value, outcome: Outcome) -> Option<&Value> {
    data["checkpoint"]["nodes"].as_array()?.iter().find(|node| {
        node["advance_note"].is_string()
            // A dependant blocked by an upstream failure is not a launch
            // error; the shared cause rule keeps `run` from naming it (#4102).
            && !is_upstream_blocked(
                node["advance_code"].as_str(),
                node["advance_note"].as_str(),
            )
            && node["attempts"].as_array().is_some_and(Vec::is_empty)
            && (matches!(outcome, Outcome::Failed | Outcome::Refused)
                || node["state"] == "failed"
                || (node["state"] == "blocked"
                    && matches!(
                        node["advance_code"].as_str(),
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

fn config_cause<'a>(
    mut error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a provenant_config::ConfigError> {
    loop {
        if let Some(config) = error.downcast_ref::<provenant_config::ConfigError>() {
            return Some(config);
        }
        if let Some(provenant_cli::fabric::EdgeError::Config(config)) =
            error.downcast_ref::<provenant_cli::fabric::EdgeError>()
        {
            return Some(config);
        }
        error = error.source()?;
    }
}

fn config_repair(globals: &Globals) -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::output::command_prefix(globals, &cwd)
        .runnable("check config --json")
        .unwrap_or_else(|| "provenant check config --json".to_owned())
}

fn config_projection(
    error: &provenant_config::ConfigError,
    globals: &Globals,
) -> provenant_records::diagnostic::Diagnostic {
    use provenant_records::diagnostic::{Destination, Diagnostic};
    if let Some(diagnostic) =
        provenant_fabric_runtime::coordination::facade::FacadeError::project_binding_directory_diagnostic(
            error,
            Destination::Local,
        )
    {
        diagnostic
    } else {
        Diagnostic::new("CONFIG")
            .category("configuration")
            .detail(error.to_string())
            .next(config_repair(globals))
    }
}

fn file_input_error(path: &Path) -> Box<provenant_records::diagnostic::Diagnostic> {
    file_error("--file", path, "input could not be read")
}

fn file_error(
    flag: &str,
    path: &Path,
    reason: &str,
) -> Box<provenant_records::diagnostic::Diagnostic> {
    cli_input_error(&format!("{flag} {}: {reason}", path.display()))
}

/// Compiled defaults from `config/harness-knobs.json`. The launch edge reads
/// the configured value when one is present; these keep fresh-context and
/// timeout handling deterministic when it is not.
const CONTEXT_MAX_BYTES: usize = 262_144;
const RUN_TIMEOUT_SECONDS: u64 = 1800;

/// Read one integer knob at the command edge. An unreadable configuration is
/// advisory: the compiled default keeps the launch bounded.
fn knob_u64(globals: &Globals, key: &str, default: u64) -> u64 {
    crate::roots::resolve(
        globals,
        crate::roots::Workspace::Directory,
        crate::roots::Product::Optional,
    )
    .ok()
    .and_then(|resolved| provenant_cli::config_reads::read_for(&resolved.roots, "run").ok())
    .and_then(|config| config.knobs.get(key).cloned())
    .and_then(|sourced| {
        sourced
            .value
            .as_u64()
            .or_else(|| sourced.value.as_str().and_then(|text| text.parse().ok()))
    })
    .unwrap_or(default)
}

fn knob_string(globals: &Globals, key: &str) -> Option<String> {
    crate::roots::resolve(
        globals,
        crate::roots::Workspace::Directory,
        crate::roots::Product::Optional,
    )
    .ok()
    .and_then(|resolved| provenant_cli::config_reads::read_for(&resolved.roots, "run").ok())
    .and_then(|config| config.knobs.get(key).cloned())
    .and_then(|sourced| sourced.value.as_str().map(str::to_owned))
}

/// Resolve inline versus handle launch. An explicit `--wait` or `--no-wait`
/// wins, then the `lanes.launch.mode` knob, then the template's shape: a leaf
/// runs inline, a composite or queue returns a handle.
fn launch_mode(wait: bool, no_wait: bool, knob: Option<&str>, composite: bool) -> LaunchMode {
    if wait {
        return LaunchMode::Inline;
    }
    if no_wait {
        return LaunchMode::Handle;
    }
    match knob {
        Some("inline") => LaunchMode::Inline,
        Some("handle") => LaunchMode::Handle,
        _ if composite => LaunchMode::Handle,
        _ => LaunchMode::Inline,
    }
}

/// A queue or composite template returns a handle; a leaf runs inline. The
/// shape is read from the flags and the graph document at this edge, before
/// the host compiles the template.
fn is_composite(template: Option<&str>, over: &[String], document_nodes: usize) -> bool {
    !over.is_empty()
        || document_nodes > 1
        || template.is_some_and(|name| {
            matches!(name, "deliver" | "deliver.batch" | "batch")
                || name.starts_with("queue")
                || name.ends_with("-swarm")
                || name == "review-panel"
                || name == "review-shape"
        })
}

/// Append opt-in context to a brief, cutting the whole result at `limit`
/// bytes. The brief is written first so a large context file cannot displace
/// the caller's own instruction. Returns whether text was cut.
fn bounded_context(brief: &str, sections: &[(String, String)], limit: usize) -> (String, bool) {
    let mut text = String::new();
    text.push_str(brief);
    for (label, body) in sections {
        if body.trim().is_empty() {
            continue;
        }
        text.push_str("\n\n--- ");
        text.push_str(label);
        text.push_str(" ---\n");
        text.push_str(body);
    }
    if text.len() <= limit {
        return (text, false);
    }
    let mut cut = limit;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    (text, true)
}

/// Read one named shared-note stream. Notes are advisory context: any failure
/// leaves the brief untouched and the caller warns once.
#[allow(clippy::disallowed_methods)]
fn named_notes(globals: &Globals, name: &str) -> Option<String> {
    use provenant_core::consumer_env::PARENT_ATTEMPT_ID;
    use provenant_fabric_runtime::notes::{Host, Input, Scope};
    let cancel = CancelToken::new();
    let input = Input::Read {
        reader: name.to_owned(),
        scope: Scope::Project,
        after: Some(0),
        limit: None,
    };
    let page = if std::env::var_os(PARENT_ATTEMPT_ID).is_some() {
        let attempt = std::env::var(PARENT_ATTEMPT_ID).ok()?.parse().ok()?;
        provenant_cli::notes::from_lane(attempt, input, &cancel).ok()?
    } else {
        let clock = provenant_os::clock::SystemClock::new();
        let resolved = crate::roots::resolve(
            globals,
            crate::roots::Workspace::Project,
            crate::roots::Product::Optional,
        )
        .ok()?;
        let ctx =
            provenant_cli::fabric::Context::open(resolved.roots, &resolved.home, &resolved.project)
                .ok()?;
        let host = Host::chair(ctx.identity.project, None);
        provenant_cli::notes::execute_host(&ctx, &host, input, &clock, &cancel).ok()?
    };
    let mut lines = Vec::new();
    for event in page["events"].as_array()? {
        if let Some(text) = event["detail"]["text"].as_str() {
            lines.push(text.to_owned());
        }
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn context_warning(detail: String) -> Warning {
    Warning::new(
        provenant_records::warning::ARGUMENT_VALUE_SUSPECT,
        detail,
        provenant_records::warning::detected_source(
            provenant_records::warning::ARGUMENT_VALUE_SUSPECT,
        ),
    )
}

/// Gather the opt-in context sections and inline them into the brief under one
/// bound. A missing context file or an empty note stream is advisory: it warns
/// and the other layers still launch.
fn inline_context(
    brief: &str,
    files: &[PathBuf],
    notes: &[String],
    globals: &Globals,
) -> (String, bool) {
    let limit = knob_u64(globals, "lanes.context_max_bytes", CONTEXT_MAX_BYTES as u64) as usize;
    let mut sections = Vec::new();
    for path in files {
        match std::fs::read_to_string(path) {
            Ok(text) => sections.push((format!("context: {}", path.display()), text)),
            Err(_) => warn(context_warning(format!(
                "--context {}: file could not be read",
                path.display()
            ))),
        }
    }
    for name in notes {
        if let Some(text) = named_notes(globals, name) {
            sections.push((format!("notes: {name}"), text));
        } else {
            warn(context_warning(format!(
                "--notes {name}: no shared notes were read"
            )));
        }
    }
    bounded_context(brief, &sections, limit)
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

/// A preparation refusal is identified by the latest attempt outcome; the
/// advance code may instead name the sealed stop cause.
pub(crate) fn is_preparation_refusal(node: &Value) -> bool {
    node["attempts"]
        .as_array()
        .and_then(|attempts| attempts.last())
        .is_some_and(|attempt| attempt["outcome"]["preparation_refused"].is_string())
}

/// One line per node the owner refused before admission, within the receipt's
/// row budget. A run that launched cleanly prints none.
fn refusal_lines(checkpoint: &Value) -> String {
    let mut text = String::new();
    let refused: Vec<String> = checkpoint["nodes"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter(|node| is_preparation_refusal(node))
        .filter_map(|node| {
            let message = node["advance_note"].as_str()?;
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

fn format_elapsed(ms: u64) -> String {
    let seconds = ms / 1000;
    format!("{}m{:02}s", seconds / 60, seconds % 60)
}

/// The inline launch footer: one bounded line naming the handle, model,
/// elapsed wall time, access and read source, and the warning count. A handle
/// launch omits it because no answer is on this invocation.
fn run_footer(data: &Value, input: &Value) -> String {
    let name = data["checkpoint"]["graph_id"]
        .as_str()
        .or_else(|| input["name"].as_str())
        .unwrap_or("-");
    let model = data["route"]["model"]
        .as_str()
        .or_else(|| input["model"].as_str())
        .unwrap_or("-");
    let elapsed = data["elapsed_ms"]
        .as_u64()
        .map_or_else(|| "-".to_owned(), format_elapsed);
    let access = input["access"].as_str().unwrap_or("read-only");
    let source = input["source"].as_str().unwrap_or("checkout");
    let warnings = data["warnings"].as_array().map_or(0, Vec::len);
    provenant_cli::bounded_text(
        &format!("— {name} · {model} · {elapsed} · {access}, {source} · {warnings} warnings"),
        provenant_cli::limits::SUMMARY_ROW_BYTES,
    )
}

/// The compact `run` result: the headline, unless it was already announced,
/// then the one next command. A refusal before admission adds its own line.
fn run_human(
    data: &Value,
    input: &Value,
    announced: bool,
) -> (String, provenant_records::command_result::NextStep) {
    let graph = data["checkpoint"]["graph_id"].as_str().unwrap_or_default();
    // The reason comes from the run's own status: a copied command is useful
    // only when the reader knows which state made it the next step.
    let (rest, why) = if data["timed_out"] == true {
        (format!("wait {graph}"), "the inline run is still running")
    } else {
        match data["status"].as_str() {
            Some("complete") => (format!("output {graph}"), "the run is complete"),
            Some("cancelled") => (format!("output {graph}"), "the run was cancelled"),
            Some("paused") => (format!("status {graph}"), "the run is paused"),
            _ => (format!("wait {graph}"), "the run is still running"),
        }
    };
    let next = provenant_records::command_result::NextStep::from_command(
        &format!("provenant {rest}"),
        why,
    );
    let status = data["status"].as_str().unwrap_or("pending");
    let document = input.get("document");
    // A changed input under a known name starts a suffixed graph. Say so in
    // the first line rather than burying it after the headline; the announce
    // callback prints the same line at launch time so it leads in both paths.
    let notice = data["named"]["superseded"]
        .as_str()
        .filter(|_| data["named"]["graph"].is_string())
        .map(|superseded| format!("started {graph} ({superseded} exists with a different input)\n"))
        .unwrap_or_default();
    let mut lanes = String::new();
    lanes += &refusal_lines(&data["checkpoint"]);
    if let Some(accesses) = data["lane_accesses"].as_array() {
        for item in accesses {
            if let Ok(access) =
                serde_json::from_value::<provenant_records::LaneAccessReceipt>(item.clone())
            {
                lanes += &format!(
                    "access: {} ({} {})\n",
                    access.access.as_str(),
                    access.key,
                    access.source.as_str()
                );
            }
        }
    }
    if let Some(warnings) = data["warnings"].as_array() {
        for warning in warnings.iter().filter_map(Value::as_str) {
            lanes += &format!("warning: {warning}\n");
        }
    }
    // A handle launch returns at admission and prints only the handle; an
    // inline launch carries its final answer and closes with the one-line
    // footer before the rendered next step.
    let footer = if input["launch_mode"].as_str() == Some("handle") {
        String::new()
    } else {
        format!("{}\n", run_footer(data, input))
    };
    if announced {
        (format!("{lanes}{footer}"), next)
    } else {
        let headline = run_headline(&data["checkpoint"], status, document, input);
        (format!("{notice}{headline}\n{lanes}{footer}"), next)
    }
}

// The graph command's one-line announcement is written to stdout as its protocol.
#[allow(clippy::disallowed_methods)]
fn invoke(
    command: &str,
    input: Result<Value, Box<provenant_records::diagnostic::Diagnostic>>,
    globals: &Globals,
    workers: &'static dyn WorkerObserver,
    timeout_seconds: Option<u64>,
) -> u8 {
    // `--timeout` bounds the whole invocation; nested waits receive what is
    // left of it through a smaller launch budget.
    let cap = |seconds: u64| timeout_seconds.map_or(seconds, |timeout| seconds.min(timeout));
    let input = match input {
        Ok(input) => input,
        Err(projection) => {
            let mut result = CommandResult::new(command, Outcome::Invalid, Value::Null);
            let repair = projection.next.clone().unwrap_or_else(|| {
                "correct the reported graph input, then rerun the graph command".to_owned()
            });
            let mut error = provenant_records::CommandError::new(
                projection.code.clone(),
                projection
                    .detail
                    .clone()
                    .unwrap_or_else(|| "graph input could not be processed".into()),
                None,
                &repair,
            );
            error.set_diagnostic(Some(projection.as_ref().into()));
            result.error = Some(error);
            return crate::emit(&result, globals, "", false);
        }
    };
    let announced = std::cell::Cell::new(false);
    let result = (|| -> Result<Value, Box<dyn std::error::Error>> {
        if command == "run"
            && std::env::var_os(provenant_core::consumer_env::PARENT_ATTEMPT_ID).is_some()
        {
            let factory = fabric::read_factory(globals, workers)?;
            let wait_seconds = cap(input["wait_seconds"]
                .as_u64()
                .unwrap_or(factory.context.config.graph.launch_wait_seconds));
            let budget = WaitBudget::cli(factory.clock.mono_now(), wait_seconds)?;
            return Ok(factory.relay_document(&input, budget, &CancelToken::new())?);
        }
        let factory = if input["explain"] == true || command == "templates" {
            fabric::read_factory(globals, workers)?
        } else {
            fabric::factory(globals, workers)?
        };
        // `run` takes no wait: its launch, including binding a writer, has the
        // configured launch allowance, bounded by `--timeout`.
        let wait_seconds = cap(input["wait_seconds"]
            .as_u64()
            .unwrap_or(factory.context.config.graph.launch_wait_seconds));
        let budget = WaitBudget::cli(factory.clock.mono_now(), wait_seconds)?;
        let local_branch = (command == "run" && input["host"].is_null())
            .then(|| input["branch"].as_str().map(str::to_owned))
            .flatten();
        // The compact view names the lane as soon as its checkpoint is saved.
        let announce =
            |document: &provenant_fabric_runtime::graph::GraphDocument,
             checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint| {
                let requested = input["name"]
                    .as_str()
                    .or_else(|| input["document"]["graph_id"].as_str())
                    .or_else(|| input["graph_id"].as_str());
                let line = match requested.filter(|name| *name != checkpoint.graph_id) {
                    Some(superseded) => format!(
                        "started {} ({} exists with a different input)\n",
                        checkpoint.graph_id, superseded
                    ),
                    None => {
                        let document = serde_json::to_value(document).unwrap_or_default();
                        let checkpoint = serde_json::to_value(checkpoint).unwrap_or_default();
                        run_headline(&checkpoint, "pending", Some(&document), &input) + "\n"
                    }
                };
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
                            && node["advance_code"] == "writer_binding_unavailable"
                            && node["advance_owner_code"] != "writer_lock_busy"
                    })
                })
            && let Some(action) = local_branch_action(&factory, branch)
        {
            result["local_worktree_remedy"] = json!(action);
        }
        factory.name_refusal_causes(&mut result);
        if command == "run"
            && let Some(graph) = result["checkpoint"]["graph_id"].as_str()
            && let Ok(target) = provenant_cli::Target::parse(graph)
        {
            let edge = provenant_cli::run::Edge {
                roots: &factory.context.roots,
                home: &factory.home,
                assets: &factory.assets,
                cwd: &factory.cwd,
            };
            let mut session = None;
            if let Ok(rows) = fabric::final_status_rows(&target, &edge, &mut session) {
                let accesses: Vec<_> = rows
                    .into_iter()
                    .filter(|row| row["kind"] == "attempt")
                    .filter_map(|row| row.pointer("/applied_route/lane_access").cloned())
                    .collect();
                if !accesses.is_empty() {
                    result["lane_accesses"] = json!(accesses);
                }
            }
        }
        Ok(result)
    })();
    match result {
        Ok(mut data) => {
            if command == "run"
                && data["timed_out"] == true
                && input["launch_mode"].as_str() != Some("handle")
            {
                data["still_running"] = json!(true);
            }
            let (human, next) = if command == "run"
                && !globals.full
                && data["checkpoint"]["graph_id"].is_string()
            {
                let (human, step) = run_human(&data, &input, announced.get());
                (human, Some(step))
            } else {
                (
                    serde_json::to_string_pretty(&data).unwrap_or_default() + "\n",
                    None,
                )
            };
            // A replay reports the graph its input already started; its failure
            // was reported when it happened.
            let replayed = data["named"]["replayed"] == true;
            let outcome = if command == "run"
                && data["timed_out"] == true
                && input["launch_mode"].as_str() == Some("handle")
            {
                // A handle returns at admission; it never reports the inline
                // timeout outcome even though its launch budget was short.
                Outcome::Ok
            } else {
                response_outcome(command, &data)
            };
            let mut result = CommandResult::new(command, outcome, data);
            result.next = next.into_iter().collect();
            if command == "run"
                && let Some(refusal) = result.data["source_admission"]
                    .as_array()
                    .and_then(|rows| rows.iter().find(|row| row["status"] == "refused"))
            {
                result.error = Some(provenant_records::CommandError::new(
                    refusal["code"].as_str().unwrap_or("preparation_refused"),
                    refusal["reason"]
                        .as_str()
                        .unwrap_or("source admission refused"),
                    None,
                    "correct the reported source admission refusal and rerun `provenant run`",
                ));
            }
            if command == "run"
                && !replayed
                && let Some(node) = launch_error_node(&result.data, result.status)
            {
                result.error = Some(provenant_records::CommandError::new(
                    node["advance_owner_code"]
                        .as_str()
                        .or_else(|| node["advance_code"].as_str())
                        .unwrap_or("launch_failed"),
                    node["advance_note"].as_str().unwrap_or("launch failed"),
                    None,
                    "correct the reported launch failure and rerun `provenant run`",
                ));
            }
            let summary = if command == "templates" {
                templates_summary(&result.data, input.get("name").and_then(Value::as_str))
            } else {
                None
            };
            crate::emit_summarised(
                &result,
                globals,
                &human,
                command == "templates",
                summary.as_ref(),
            )
        }
        Err(error) => {
            let config = config_cause(error.as_ref());
            let facade =
                error.downcast_ref::<provenant_fabric_runtime::coordination::facade::FacadeError>();
            let projection = facade
                .map(|error| error.diagnostic())
                .or_else(|| config.map(|cause| config_projection(cause, globals)))
                .or_else(|| {
                    error
                        .downcast_ref::<provenant_cli::fabric::EdgeError>()
                        .and_then(provenant_cli::fabric::edge_diagnostic)
                })
                .unwrap_or_else(|| {
                    provenant_records::diagnostic::Diagnostic::new("graph_error")
                        .detail(error.to_string())
                        .next(format!(
                            "resolve the reported graph error, then rerun `provenant {command}`"
                        ))
                });
            let outcome = facade.map_or(
                if config.is_some() {
                    Outcome::Invalid
                } else {
                    Outcome::Error
                },
                provenant_cli::fabric::coordination_outcome,
            );
            let mut result = CommandResult::new(command, outcome, Value::Null);
            let repair = projection.next.clone().unwrap_or_else(|| {
                "correct the reported graph error, then rerun the graph command".to_owned()
            });
            let cause = facade
                .and_then(|error| error.cause())
                .map_or_else(|| error.to_string(), str::to_owned);
            let mut error = provenant_records::CommandError::new(
                projection.code.as_str(),
                projection
                    .detail
                    .clone()
                    .unwrap_or_else(|| "graph input could not be processed".into()),
                Some(cause.as_str()),
                &repair,
            );
            error.set_diagnostic(Some((&projection).into()));
            result.error = Some(error);
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
        warn(Warning::new(
            provenant_records::warning::ARGUMENT_VALUE_SUSPECT,
            format!(
                "--over creates {} nodes; check comma-separated items",
                request.over.len()
            ),
            provenant_records::warning::detected_source(
                provenant_records::warning::ARGUMENT_VALUE_SUSPECT,
            ),
        ));
    }
    // `run smoke` is the hermetic lifecycle, not a graph against the caller's
    // checkout. The lifecycle re-execs this binary with a fixture project; the
    // child then lands here with a project under fixture_root and launches the
    // compiled graph normally. An explanation is a pure compile and stays on
    // that path too.
    if request.template.as_deref() == Some("smoke")
        && !request.explain
        && request.file.is_none()
        && let Ok(resolved) = crate::roots::resolve(
            globals,
            crate::roots::Workspace::Directory,
            crate::roots::Product::Optional,
        )
    {
        let (smoke_settings, smoke_origin) = crate::smoke::settings(&resolved.roots);
        let root = smoke_settings.root();
        if !resolved
            .project
            .starts_with(crate::smoke::fixture_root(&resolved.home, &root))
        {
            if globals.project.is_some() {
                warn(Warning::new(
                    provenant_records::warning::SMOKE_PROJECT_IGNORED,
                    "run smoke ignores --project and uses a hermetic fixture",
                    provenant_records::warning::detected_source(
                        provenant_records::warning::SMOKE_PROJECT_IGNORED,
                    ),
                ));
            }
            return smoke_lifecycle(&request, globals, &resolved, &smoke_settings, &smoke_origin);
        }
    }
    // An explanation compiles against the caller's project and creates no
    // fixture. Report the effective smoke values and their sources alongside it.
    if request.template.as_deref() == Some("smoke")
        && request.explain
        && let Ok(resolved) = crate::roots::resolve(
            globals,
            crate::roots::Workspace::Directory,
            crate::roots::Product::Optional,
        )
    {
        let (smoke_settings, smoke_origin) = crate::smoke::settings(&resolved.roots);
        warn(Warning::new(
            provenant_records::warning::LAUNCH_ADVISORY,
            format!(
                "run smoke --explain effective settings:\n{}",
                smoke_settings.explain(&smoke_origin)
            ),
            provenant_records::warning::detected_source(
                provenant_records::warning::LAUNCH_ADVISORY,
            ),
        ));
    }
    let bare_model = request
        .model
        .as_deref()
        .is_some_and(|model| !model.contains('/'));
    let catalogue = if bare_model {
        crate::roots::resolve(
            globals,
            crate::roots::Workspace::Directory,
            crate::roots::Product::Optional,
        )
        .map_err(|error| cli_input_error(&error.to_string()))
        .and_then(|resolved| {
            provenant_cli::route::effective_catalogue(&resolved.roots)
                .map_err(|error| cli_input_error(&error.to_string()))
        })
    } else {
        provenant_routing::Catalogue::bundled().map_err(|error| cli_input_error(&error.to_string()))
    };
    // Issue #3724: name a catalogued ID whose provider segment the caller
    // omitted before dispatch can refuse the malformed model. The launch
    // preparation repeats this against the effective (discovered) catalogue.
    if let Some(id) = request
        .model
        .as_deref()
        .and_then(|model| provenant_routing::ModelId::parse(model).ok())
    {
        let warning = catalogue
            .as_ref()
            .ok()
            .and_then(|catalogue| catalogue.provider_segment_warning(&id))
            .or_else(|| {
                // A discovered exact model is absent from the bundled
                // catalogue. This read is advisory: it never changes the
                // catalogue dispatch uses or refuses the request.
                crate::roots::resolve(
                    globals,
                    crate::roots::Workspace::Directory,
                    crate::roots::Product::Optional,
                )
                .ok()
                .and_then(|resolved| {
                    provenant_cli::route::effective_catalogue(&resolved.roots).ok()
                })
                .and_then(|catalogue| catalogue.provider_segment_warning(&id))
            });
        if let Some(warning) = warning {
            warn(Warning::new(
                provenant_records::warning::MODEL_SEGMENT_ADVISORY,
                warning,
                provenant_records::warning::detected_source(
                    provenant_records::warning::MODEL_SEGMENT_ADVISORY,
                ),
            ));
        }
    }
    let timeout_seconds = request.common.timeout_seconds();
    let briefs = request.brief_text.clone();
    if briefs.len() > 1 {
        // Fan-out: one lane per brief. Each result is printed under a bounded
        // header and the worst status wins, so a single failed lane is not
        // hidden by the others. The host runs the lanes concurrently under
        // its own admission cap; this edge keeps their output ordered.
        let total = briefs.len();
        let mut worst = 0u8;
        for (index, brief) in briefs.into_iter().enumerate() {
            let mut one = request.clone();
            one.brief = Some(brief.clone());
            one.brief_text.clear();
            one.brief_file = None;
            let header = format!(
                "== brief {}/{}: {} ==\n",
                index + 1,
                total,
                provenant_cli::bounded_text(&brief, provenant_cli::limits::SUMMARY_ROW_BYTES)
            );
            let _ = crate::output::write_header(&header);
            let input = match catalogue.as_ref() {
                Ok(cat) => run_input(one, cat, globals),
                Err(error) => Err(error.clone()),
            };
            worst = worst.max(invoke("run", input, globals, workers, timeout_seconds));
        }
        return worst;
    }
    invoke(
        "run",
        catalogue.and_then(|catalogue| run_input(request, &catalogue, globals)),
        globals,
        workers,
        timeout_seconds,
    )
}

fn smoke_host(common: &provenant_cli::CommonArgs) -> Result<Option<String>, &'static str> {
    match common.target() {
        provenant_cli::HostSelection::Local => Ok(None),
        provenant_cli::HostSelection::Named(host) => Ok(Some(host)),
        provenant_cli::HostSelection::Many(_) | provenant_cli::HostSelection::All => {
            Err("run accepts at most one named host")
        }
    }
}

/// Drive the smoke lifecycle from the CLI edge and publish its typed output.
/// A failed lifecycle carries the failing row named by the verifier; a passed
/// one carries the child's launch, wait and status envelopes.
fn smoke_lifecycle(
    request: &GraphRunArgs,
    globals: &Globals,
    resolved: &crate::roots::Resolved,
    settings: &provenant_cli::smoke::Settings,
    origin: &provenant_cli::smoke::Origin,
) -> u8 {
    let program = match std::env::current_exe() {
        Ok(program) => program,
        Err(error) => {
            return cli_input_error_emit(
                "run",
                &format!("smoke program is unavailable: {error}"),
                globals,
            );
        }
    };
    let runner = match provenant_os::exec::SystemExecutor::new(1) {
        Ok(executor) => crate::install::ChildRunner {
            executor,
            environment: std::env::vars_os().collect(),
            cwd: resolved.project.clone(),
        },
        Err(error) => {
            return cli_input_error_emit(
                "run",
                &format!("smoke executor is unavailable: {error}"),
                globals,
            );
        }
    };
    let host = match smoke_host(&request.common) {
        Ok(host) => host,
        Err(detail) => return cli_input_error_emit("run", detail, globals),
    };
    let args = provenant_cli::install::Upgrade {
        common: request.common.clone(),
        keep: false,
        yes: false,
        from_source: None,
        hosts: "all".into(),
        verify: true,
        verify_only: false,
        dry_run: false,
        host,
        route: request.route.clone(),
        set: request.set.clone(),
    };
    let clock = provenant_os::clock::SystemClock::new();
    match crate::smoke::run_lifecycle(
        &clock,
        &runner,
        &program,
        resolved.roots.state.as_path(),
        &resolved.home,
        &args,
    ) {
        Ok(outcome) => match &outcome.verdict {
            crate::smoke::SmokeVerdict::Passed => {
                let text = outcome.text();
                crate::emit(
                    &CommandResult::new("run", Outcome::Ok, serde_json::json!({"smoke": text})),
                    globals,
                    "",
                    false,
                )
            }
            crate::smoke::SmokeVerdict::Unverified { cause, .. } => {
                let text = outcome.text();
                if settings.unverified == provenant_cli::smoke::Unverified::Fail {
                    let mut result =
                        CommandResult::new("run", Outcome::Failed, serde_json::Value::Null);
                    result.error = Some(provenant_records::CommandError::new(
                        cause.code().0,
                        format!(
                            "{text}\nsource: {}",
                            origin.describe(provenant_cli::smoke::UNVERIFIED_KEY)
                        ),
                        None,
                        cause.remedy(),
                    ));
                    return crate::emit(&result, globals, "", false);
                }
                crate::emit(
                    &CommandResult::new("run", Outcome::Ok, serde_json::json!({"smoke": text})),
                    globals,
                    "",
                    false,
                )
            }
            crate::smoke::SmokeVerdict::Failed { .. } => {
                let mut result =
                    CommandResult::new("run", Outcome::Failed, serde_json::Value::Null);
                result.error = Some(provenant_records::CommandError::new(
                    "SMOKE_FAILED",
                    outcome.text(),
                    None,
                    "inspect the reported smoke failure, then rerun `provenant run smoke`",
                ));
                crate::emit(&result, globals, "", false)
            }
        },
        Err(error) => {
            let mut result = CommandResult::new("run", Outcome::Failed, serde_json::Value::Null);
            result.error = Some(provenant_records::CommandError::new(
                "SMOKE_FAILED",
                &error,
                None,
                "inspect the reported smoke failure, then rerun `provenant run smoke`",
            ));
            crate::emit(&result, globals, "", false)
        }
    }
}

fn cli_input_error_emit(command: &str, detail: &str, globals: &Globals) -> u8 {
    let mut result = CommandResult::new(command, Outcome::Invalid, serde_json::Value::Null);
    result.error = Some(provenant_records::CommandError::new(
        "INVALID",
        detail,
        None,
        "resolve the reported input error and retry",
    ));
    crate::emit(&result, globals, "", false)
}

/// A `--model` without an adapter names the one catalogued model that carries
/// it. `ADAPTER/ID` passes through unchanged for the facade to check.
fn model_selector(
    model: String,
    catalogue: &provenant_routing::Catalogue,
) -> Result<String, Box<provenant_records::diagnostic::Diagnostic>> {
    if model.contains('/') {
        return Ok(model);
    }
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
        BareModelError::Unknown { candidates } => {
            let mut detail = format!(
                "unknown model {shown}; no catalogued provider offers it, so name it in the provider/model form"
            );
            if !candidates.is_empty() {
                detail.push_str("; close matches: ");
                detail.push_str(&candidates.join(", "));
            }
            detail
        }
    }
}

fn implementation_template(name: &str) -> bool {
    name == "implement" || name.starts_with("implement.")
}

fn run_input(
    request: GraphRunArgs,
    catalogue: &provenant_routing::Catalogue,
    globals: &Globals,
) -> Result<Value, Box<provenant_records::diagnostic::Diagnostic>> {
    (|| {
        // Capture the native-lane launch flags before the request's other
        // fields are moved into the graph input.
        let context_files = request.context.clone();
        let note_names = request.notes.clone();
        let strict = request.strict;
        let checks = request.checks.clone();
        let isolated = request.isolated;
        let destination = request.destination.clone();
        let commit = request.commit.clone();
        let worktree_sharing = request.worktree_sharing.clone();
        let wait = request.wait;
        let no_wait = request.no_wait;
        let timeout_seconds = request.common.timeout_seconds();
        let plan = request.plan;
        let implementation = request.template.as_deref().is_some_and(|name| {
            implementation_template(name) || name == "deliver" || name == "batch"
        });
        let sandbox = request.sandbox_override();
        let brief_source = select_brief_source([
            request.brief.map(BriefSource::Text),
            request.brief_text.into_iter().next().map(BriefSource::Text),
            request.brief_file.map(BriefSource::file),
        ])
        .map_err(|error| cli_input_error(&error.to_string()))?;
        let model = request
            .model
            .map(|model| model_selector(model, catalogue))
            .transpose()?;
        let mut input = json!({"op":"run","explain":request.explain,"detached":request.detached});
        if let Some(path) = request.preamble_file {
            input["preamble_file"] = json!(path);
        }
        for (field, value) in [
            (
                "host",
                request.common.host.first().map(|host| match host {
                    provenant_cli::HostArg::All => "all".to_owned(),
                    provenant_cli::HostArg::Named(name) => name.clone(),
                }),
            ),
            ("model", model),
            ("effort", request.effort),
            ("account", request.account),
            ("name", request.name),
            ("template", request.template),
            ("route", request.route),
            ("source", request.source),
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
        if let Some(pr) = request.pr {
            input["pr"] = json!(pr);
        }
        if let Some(head) = request.head {
            input["head"] = json!(head);
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
        let mut review_policy = json!({});
        for item in request.set {
            let (key, value) = item
                .split_once('=')
                .ok_or_else(|| cli_input_error("--set expects NAME=VALUE"))?;
            if key.is_empty() {
                return Err(cli_input_error("--set parameter name is empty"));
            }
            validate_set_path(key)?;
            if let Some(field) = key.strip_prefix("domains.review.") {
                let value = toml_edit::de::from_str::<Value>(&format!("value = {value}"))
                    .map(|parsed| parsed["value"].clone())
                    .unwrap_or_else(|_| json!(value));
                review_policy[field] = value;
            } else if key == "attempt" {
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
        if plan && implementation {
            parameters["plan"] = json!(true);
        }
        let mut policy_inputs = Vec::new();
        if let Some(BriefSource::File(path)) = &brief_source {
            policy_inputs.push(std::path::absolute(path).map_err(|_| file_input_error(path))?);
        }
        if let Some(path) = &request.file {
            policy_inputs.push(std::path::absolute(path).map_err(|_| file_input_error(path))?);
        }
        if !policy_inputs.is_empty() {
            parameters["attempt.policy_inputs"] = json!(policy_inputs);
        }
        if review_policy
            .as_object()
            .is_some_and(|policy| !policy.is_empty())
        {
            input["review_policy"] = review_policy;
        }
        input["set"] = parameters;
        if let Some(source) = brief_source {
            let brief = read_brief(source, provenant_cli::brief::MAX_BRIEF_BYTES)
                .map_err(|error| cli_input_error(&error.to_string()))?;
            if brief.trim().is_empty() {
                return Err(cli_input_error("brief is empty"));
            }
            // Fresh context is the brief plus the standing preamble (carried
            // on the attempt) and only the explicitly named files and notes.
            let (brief, truncated) = inline_context(&brief, &context_files, &note_names, globals);
            if truncated {
                warn(context_warning(format!(
                    "context exceeded {} bytes and was cut; --full lists the sources",
                    CONTEXT_MAX_BYTES
                )));
            }
            input["brief"] = json!(brief);
        }
        input["context_inputs"] = serde_json::to_value(ContextInputs {
            files: context_files
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            notes: note_names,
        })
        .unwrap_or_default();
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
        // Launch mode comes from explicit flags, then the knob, then the
        // template's shape. A composite or queue returns a handle, so its
        // launch does not block the caller for the whole lane.
        let over: Vec<String> = input["over"].as_array().map_or_else(Vec::new, |rows| {
            rows.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        });
        let composite = !over.is_empty()
            || input["over"].is_string()
            || is_composite(
                input["template"].as_str(),
                &over,
                input["document"]["nodes"].as_array().map_or(0, Vec::len),
            );
        let mode = launch_mode(
            wait,
            no_wait,
            knob_string(globals, "lanes.launch.mode").as_deref(),
            composite,
        );
        input["launch_mode"] = serde_json::to_value(mode).unwrap_or_default();

        // `--strict`, `--checks` and `--isolated` select launch-layer policy.
        // The host applies `checks_mode` with `Policy::apply_checks`, which
        // cannot raise a boundary or operation row.
        let checks_mode = if strict {
            Some("strict".to_owned())
        } else {
            checks
                .filter(|value| provenant_records::policy::ChecksMode::from_checks(value).is_some())
        };
        if let Some(value) = checks_mode {
            input["checks_mode"] = json!(value);
        }
        if isolated {
            input["isolated"] = json!(true);
            input["sandbox"] = json!("on");
            input["destination"] = json!("new");
            input["worktree_sharing"] = json!("refuse");
            input["checks_mode"] = json!("strict");
        }
        if let Some(value) = destination {
            input["destination"] = json!(value);
        }
        if let Some(value) = commit {
            input["commit"] = json!(value);
        }
        if let Some(value) = worktree_sharing {
            input["worktree_sharing"] = json!(value);
        }
        let timeout = timeout_seconds
            .unwrap_or_else(|| knob_u64(globals, "domains.cli.timeouts.run", RUN_TIMEOUT_SECONDS));
        input["timeout_seconds"] = json!(timeout);
        if matches!(mode, LaunchMode::Handle) && !wait {
            input["wait_seconds"] = json!(0);
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
        | "ci_scope" | "pr" | "preset" | "receipts" | "sandbox" | "policy_inputs"
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
        None,
    )
}
pub(crate) fn templates_summary(
    data: &Value,
    show_target: Option<&str>,
) -> Option<provenant_cli::Summary> {
    let entries = data.get("templates")?.as_array()?;
    if let Some(target) = show_target {
        let entry = entries.iter().find(|e| e["name"] == target)?;
        let name = entry["name"].as_str().unwrap_or(target);
        let layer = entry["layer"].as_str().unwrap_or("unknown");
        let template = &entry["template"];

        let nodes_str = if let Some(nodes) = template
            .get("nodes")
            .and_then(Value::as_array)
            .filter(|n| !n.is_empty())
        {
            let mut parts = Vec::new();
            for node in nodes {
                let id = node["id"].as_str().unwrap_or("unknown");
                let role = node["role"]
                    .as_str()
                    .or_else(|| node["use"].as_str())
                    .or_else(|| node["kind"].as_str())
                    .or_else(|| node["tier"].as_str())
                    .unwrap_or("-");
                parts.push(format!("{id} ({role})"));
            }
            parts.join(", ")
        } else {
            let role = template["role"]
                .as_str()
                .or_else(|| template["kind"].as_str())
                .or_else(|| template["tier"].as_str())
                .unwrap_or("leaf");
            format!("{name} ({role})")
        };

        let mut param_parts = Vec::new();
        if let Some(params) = template.get("parameters").and_then(Value::as_object) {
            for (pname, pval) in params {
                if let Some(def) = pval.get("default").filter(|v| !v.is_null()) {
                    let def_str = serde_json::to_string(def).unwrap_or_else(|_| def.to_string());
                    param_parts.push(format!("{pname}={def_str}"));
                } else if !pval.is_object() && !pval.is_null() {
                    let def_str = serde_json::to_string(pval).unwrap_or_else(|_| pval.to_string());
                    param_parts.push(format!("{pname}={def_str}"));
                } else {
                    param_parts.push(format!("{pname} (no default)"));
                }
            }
        }
        param_parts.sort();
        let params_str = if param_parts.is_empty() {
            "none".to_string()
        } else {
            param_parts.join(", ")
        };

        Some(provenant_cli::Summary {
            lines: vec![
                format!("{name} ({layer})"),
                format!("nodes: {nodes_str}"),
                format!("parameters: {params_str}"),
            ],
            omitted: 0,
        })
    } else {
        let mut lines = Vec::new();
        for entry in entries {
            let name = entry["name"].as_str().unwrap_or("unknown");
            let layer = entry["layer"].as_str().unwrap_or("unknown");
            let template = &entry["template"];
            let purpose = template
                .get("doc")
                .and_then(Value::as_str)
                .or_else(|| template.get("brief").and_then(Value::as_str))
                .or_else(|| template.get("prompt").and_then(Value::as_str))
                .and_then(|text| text.lines().next())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("-");
            lines.push(format!("{layer} {name}: {purpose}"));
        }
        if lines.is_empty() {
            lines.push("no templates".to_string());
        }
        Some(provenant_cli::Summary { lines, omitted: 0 })
    }
}

pub(crate) fn templates(
    request: Templates,
    globals: &Globals,
    workers: &'static dyn WorkerObserver,
) -> u8 {
    let input = (|| {
        let mut input = json!({"op":"templates"});
        if let Some(provenant_cli::TemplatesCommand::Show { name }) = request.command {
            input["name"] = json!(name);
        }
        if let Some(path) = request.file {
            let value = document(&path)?;
            input["templates"] = value.get("templates").unwrap_or(&value).clone();
        }
        Ok(input)
    })();
    invoke("templates", input, globals, workers, None)
}
#[cfg(test)]
include!("graph_interface_tests.rs");
