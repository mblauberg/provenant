//! CLI documents for the shared graph service.
use crate::fabric;
use provenant_cli::{Edit, Globals, GraphRunArgs, GraphStats, Status, Templates};
use provenant_confinement::worktrees::WorkerObserver;
use provenant_core::{CancelToken, Clock, WaitBudget};
use provenant_records::{CommandResult, Outcome};
use serde_json::{Value, json};
use std::path::Path;

fn document(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    decode(&text)
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

fn input_diagnostic(error: impl std::fmt::Display) -> provenant_records::diagnostic::Diagnostic {
    provenant_fabric_runtime::coordination::facade::FacadeError::invalid(
        provenant_fabric_runtime::coordination::facade::InvalidInputDetail::graph_reason(error),
    )
    .diagnostic()
}

fn invoke(
    command: &str,
    input: Result<Value, String>,
    globals: &Globals,
    workers: &'static dyn WorkerObserver,
) -> u8 {
    let result = (|| -> Result<Value, Box<dyn std::error::Error>> {
        let input = input?;
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
        let wait_seconds = input["wait_seconds"].as_u64().unwrap_or(55).max(55);
        let budget = WaitBudget::cli(factory.clock.mono_now(), wait_seconds)?;
        Ok(factory.graph_document(input, budget, &CancelToken::new())?)
    })();
    match result {
        Ok(data) => {
            let human = serde_json::to_string_pretty(&data).unwrap_or_default() + "\n";
            crate::emit(
                &CommandResult::new(command, response_outcome(command, &data), data),
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
                .unwrap_or_else(|| input_diagnostic(&error));
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
pub(crate) fn run(
    request: GraphRunArgs,
    globals: &Globals,
    workers: &'static dyn WorkerObserver,
) -> u8 {
    invoke("run", run_input(request), globals, workers)
}

fn run_input(request: GraphRunArgs) -> Result<Value, String> {
    (|| {
        let mut input = json!({"op":"run","explain":request.explain,"detached":request.detached});
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
                .ok_or("--set expects NAME=TOML_VALUE")?;
            let parsed: Value = toml_edit::de::from_str(&format!("value = {value}"))
                .map_err(|error| error.to_string())?;
            if key.is_empty() {
                return Err("--set parameter name is empty".into());
            }
            parameters[key] = parsed["value"].clone();
        }
        input["set"] = parameters;
        if let Some(brief) = request.brief {
            input["brief"] = json!(brief);
        }
        if let Some(path) = request.file {
            let text = std::fs::read_to_string(&path)
                .map_err(|error| format!("--file {}: {error}", path.display()))?;
            match decode(&text) {
                Ok(value) if value.is_object() => input["document"] = value,
                Ok(_)
                    if path
                        .extension()
                        .is_some_and(|ext| ext == "json" || ext == "toml") =>
                {
                    return Err("graph document must be an object".into());
                }
                Err(error)
                    if path
                        .extension()
                        .is_some_and(|ext| ext == "json" || ext == "toml") =>
                {
                    return Err(error);
                }
                _ => input["brief"] = json!(text),
            }
        }
        Ok(input)
    })()
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
    request: Status,
    globals: &Globals,
    workers: &'static dyn WorkerObserver,
) -> u8 {
    invoke(
        "status",
        Ok(json!({"op":"status","id":request.id,"stats":true,"by":request.by})),
        globals,
        workers,
    )
}

pub(crate) fn graph_stats(
    request: GraphStats,
    globals: &Globals,
    workers: &'static dyn WorkerObserver,
) -> u8 {
    invoke(
        "stats",
        Ok(json!({"op":"status","id":request.id,"stats":true,"by":request.by})),
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
            let diagnostic = input_diagnostic(error);
            assert_eq!(diagnostic.code, "invalid_input");
            assert!(
                diagnostic
                    .detail
                    .as_deref()
                    .is_some_and(|text| text.contains(detail))
            );
        }
        let directory = tempfile::tempdir()?;
        let mut request = run_request();
        request.file = Some(directory.path().join("missing.json"));
        let error = run_input(request)
            .err()
            .ok_or("missing file was accepted")?;
        let diagnostic = input_diagnostic(error);
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
    fn run_reports_the_graph_outcome_and_status_reads_remain_successful() {
        for (status, outcome, expected) in [
            ("complete", "failed", Outcome::Failed),
            ("complete", "succeeded", Outcome::Ok),
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
