//! `adapter` — inspect the declared capability matrix.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::{Command, CommandResult};
use provenant_core::{ActorId, ProjectId, consumer_env::Stability};
use provenant_records::{CommandError, Outcome};
use serde_json::{Value, json};

pub static DOMAIN: Domain = Domain {
    name: "adapter",
    verbs: &[Verb {
        path: &["adapter", "capabilities"],
        stability: Stability::Evolving,
        run,
    }],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Adapter(provenant_cli::Adapter::Capabilities) => {
            let outcome = (|| -> Result<(Value, String), Box<dyn std::error::Error>> {
                let resolved = crate::roots::resolve(
                    globals,
                    crate::roots::Workspace::Project,
                    crate::roots::Product::Optional,
                )?;
                let project_id = provenant_os::git::repository_path(
                    &resolved.project,
                    provenant_os::git::RepositoryPath::CommonDir,
                )
                .ok()
                .and_then(|common| provenant_config::ProjectLocation::new(common).ok())
                .and_then(|location| {
                    provenant_config::lookup_project(&resolved.roots, &location)
                        .ok()
                        .flatten()
                })
                .map(|binding| binding.project_id())
                .unwrap_or_else(|| ProjectId::from_bytes([0; 16]));
                let clock = provenant_os::clock::SystemClock::new();
                let cancel = provenant_core::CancelToken::new();
                let call = provenant_run::Request {
                    clock: &clock,
                    cancel: &cancel,
                };
                let context = provenant_run::ExecutionContext {
                    state_root: resolved.roots.state.clone(),
                    project_id,
                    actor_id: ActorId::from_bytes([0; 16]),
                    read_prompts: false,
                };
                let store = provenant_run::RunStore::open_read_only(&context, &call)
                    .ok()
                    .flatten();
                let value = provenant_cli::adapter_capabilities::matrix(
                    resolved.roots.state.as_path(),
                    Some(&resolved.project),
                    store.as_ref(),
                );
                let human = human(&value);
                Ok((value, human))
            })();
            match outcome {
                Ok((value, human)) => crate::emit(
                    &CommandResult::new("adapter capabilities", Outcome::Ok, value),
                    globals,
                    &human,
                    true,
                ),
                Err(error) => crate::fail(
                    "adapter capabilities",
                    Outcome::Error,
                    "ADAPTER_CAPABILITIES",
                    &error.to_string(),
                    crate::REPAIR_ROOT,
                    globals,
                ),
            }
        }
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}

/// One block per adapter. A "no" value names the reason and its source; a yes
/// value stays compact so a terminal reader can scan the matrix.
fn human(value: &Value) -> String {
    let Some(adapters) = value["adapters"].as_array() else {
        return String::new();
    };
    let mut lines = Vec::new();
    for adapter in adapters {
        let name = adapter["adapter"].as_str().unwrap_or("unknown");
        let family = adapter["family"].as_str().unwrap_or("unknown");
        lines.push(format!("{name} ({family})"));
        let capabilities = &adapter["capabilities"];
        for (label, key) in [
            ("model", "model"),
            ("effort", "effort"),
            ("image input", "image_input"),
            ("resume", "resume"),
            ("write", "write"),
            ("terminal event", "terminal_event"),
            ("answering model", "answering_model"),
            ("usage attribution", "usage_attribution"),
            ("live input", "live_input"),
        ] {
            let declaration = if matches!(
                key,
                "terminal_event" | "answering_model" | "usage_attribution" | "live_input"
            ) {
                &capabilities[key]
            } else {
                &capabilities["launch"][key]
            };
            push_declaration(&mut lines, label, declaration);
        }
        if let Some(native) = capabilities.get("native") {
            lines.push("  native".to_owned());
            for (label, key) in [
                ("hooks", "hooks"),
                ("answer delivery", "answer_delivery"),
                ("precompact context", "precompact_context"),
                ("instructions", "instructions"),
            ] {
                push_declaration(&mut lines, &format!("    {label}"), &native[key]);
            }
        }
        let compatibility = &adapter["compatibility"];
        lines.push(format!(
            "  compatibility: {} ({})",
            compatibility["status"].as_str().unwrap_or("unknown"),
            compatibility["version"]
                .as_str()
                .unwrap_or("version not read")
        ));
        let measurements = &adapter["measurements"];
        if measurements.is_null() {
            lines.push("  measurements: no samples".to_owned());
        } else {
            lines.push(format!(
                "  measurements: launch p50 {} ms, settle p50 {} ms, {} samples",
                measurements["launch_ms_p50"],
                measurements["settle_ms_p50"],
                measurements["samples"]
            ));
        }
        lines.push(String::new());
    }
    lines.join("\n").trim_end().to_owned()
}

fn push_declaration(lines: &mut Vec<String>, label: &str, declaration: &Value) {
    let value = &declaration["value"];
    let is_no = *value == json!(false) || *value == json!("none");
    if is_no {
        lines.push(format!(
            "  {label}: no — {}; source={}",
            declaration["reason"].as_str().unwrap_or(""),
            declaration["source"].as_str().unwrap_or("")
        ));
    } else {
        let shown = match value {
            Value::Bool(true) => "yes".to_owned(),
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        lines.push(format!("  {label}: {shown}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_renders_no_values_with_reasons() {
        let value = json!({
            "adapters": [{
                "adapter": "command",
                "family": "local",
                "capabilities": {
                    "launch": {
                        "model": {"value": false, "reason": "configured commands take no model selector", "source": "row"},
                        "effort": {"value": true, "reason": "declared", "source": "row"},
                        "image_input": {"value": true, "reason": "declared", "source": "row"},
                        "resume": {"value": true, "reason": "declared", "source": "row"},
                        "write": {"value": true, "reason": "declared", "source": "row"}
                    },
                    "terminal_event": {"value": "none", "reason": "process exit is the contract", "source": "row"},
                    "answering_model": {"value": "stream", "reason": "declared", "source": "row"},
                    "usage_attribution": {"value": "turn", "reason": "declared", "source": "row"},
                    "live_input": {"value": false, "reason": "no live input", "source": "row"}
                },
                "compatibility": {"status": "unknown", "version": null},
                "measurements": null
            }]
        });
        let text = human(&value);
        assert!(text.contains("configured commands take no model selector; source=row"));
        assert!(text.contains("model: no —"));
        assert!(text.contains("effort: yes"));
        assert!(text.contains("compatibility: unknown"));
        assert!(text.contains("measurements: no samples"));
    }
}
