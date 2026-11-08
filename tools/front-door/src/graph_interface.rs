//! CLI documents for the shared graph service.
use crate::fabric;
use provenant_cli::{Edit, Globals, GraphRunArgs, Templates};
use provenant_confinement::worktrees::WorkerObserver;
use provenant_core::{CancelToken, Clock, WaitBudget};
use provenant_records::{CommandResult, Outcome};
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
            && (matches!(outcome, Outcome::Failed | Outcome::Refused) || node["state"] == "failed")
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
        let mut result = factory.graph_document(input, budget, &CancelToken::new())?;
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
        Ok(result)
    })();
    match result {
        Ok(data) => {
            let human = serde_json::to_string_pretty(&data).unwrap_or_default() + "\n";
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

fn run_input(
    request: GraphRunArgs,
) -> Result<Value, Box<provenant_records::diagnostic::Diagnostic>> {
    (|| {
        let mut input = json!({"op":"run","explain":request.explain,"detached":request.detached});
        for (field, value) in [
            ("host", request.host),
            ("model", request.model),
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
        | "write_root" => Ok(()),
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
