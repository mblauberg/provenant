//! `adapter` — inspect the declared capability matrix.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::{ActorId, ProjectId, consumer_env::Stability};
use provenant_records::{CommandError, CommandResult, Outcome};
use serde_json::Value;

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
        Command::Adapter(provenant_cli::Adapter::Capabilities { adapters, no_probe }) => {
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
                let args = provenant_cli::adapter_capabilities::MatrixArgs {
                    adapters: &adapters,
                    probe: !no_probe,
                };
                // An unreadable configuration warns and the matrix reports
                // compiled defaults, each sourced as the default layer.
                let config = match provenant_cli::config_reads::read_for(&resolved.roots, "adapter")
                {
                    Ok(config) => Some(config),
                    Err(error) => {
                        provenant_records::warning::warn(
                            provenant_records::warning::Warning::new(
                                provenant_records::warning::CONFIG_KEY_IGNORED,
                                format!(
                                    "configuration unreadable; using compiled defaults: {error}"
                                ),
                            )
                            .subject("adapter capabilities"),
                        );
                        None
                    }
                };
                let (value, warnings) = provenant_cli::adapter_capabilities::matrix_with(
                    resolved.roots.state.as_path(),
                    Some(&resolved.project),
                    config.as_ref(),
                    store.as_ref(),
                    &args,
                );
                // Under `--json` nothing goes to stderr, so skipped entries and
                // unknown ids must reach the envelope as typed warnings.
                for warning in warnings {
                    provenant_records::warning::warn(
                        provenant_records::warning::Warning::new(
                            provenant_records::warning::WarningCode("ADAPTER_CAPABILITIES_OVERLAY"),
                            warning.to_string(),
                        )
                        .subject("adapter capabilities"),
                    );
                }
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
    let mut lines = Vec::new();
    let Some(adapters) = value["adapters"].as_array() else {
        return String::new();
    };
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
            ("session reference", "session_reference"),
        ] {
            let declaration = if matches!(
                key,
                "terminal_event"
                    | "answering_model"
                    | "usage_attribution"
                    | "live_input"
                    | "session_reference"
            ) {
                &capabilities[key]
            } else {
                &capabilities["launch"][key]
            };
            push_declaration(&mut lines, label, declaration);
        }
        if let Some(caveats) = capabilities["caveats"].as_array() {
            for caveat in caveats {
                lines.push(format!(
                    "  caveat {}: {}; source={}",
                    caveat["code"].as_str().unwrap_or(""),
                    caveat["note"].as_str().unwrap_or(""),
                    caveat["source"].as_str().unwrap_or("")
                ));
            }
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
        if !adapter["overlay"].is_null() {
            let overlay = &adapter["overlay"];
            let disabled = overlay["disabled"]
                .as_array()
                .map_or_else(Vec::new, |items| {
                    items
                        .iter()
                        .map(|item| item.as_str().unwrap_or("").to_owned())
                        .collect::<Vec<_>>()
                });
            lines.push(format!(
                "  overlay: disabled={} reason={} layer={} origin={}",
                disabled.join(","),
                overlay["reason"].as_str().unwrap_or("-"),
                overlay["layer"].as_str().unwrap_or("-"),
                overlay["origin"].as_str().unwrap_or("-")
            ));
        }
        for (field, key) in [
            ("model", "model"),
            ("effort", "effort"),
            ("image input", "image_input"),
        ] {
            let rule = &adapter["on_gap"][key];
            lines.push(format!(
                "  on_gap {field}: {} (layer={} origin={})",
                rule["action"].as_str().unwrap_or("-"),
                rule["layer"].as_str().unwrap_or("-"),
                rule["origin"].as_str().unwrap_or("-")
            ));
        }
        let compatibility = &adapter["compatibility"];
        lines.push(format!(
            "  compatibility: {} ({}) layer={} origin={}",
            compatibility["status"].as_str().unwrap_or("unknown"),
            compatibility["version"]
                .as_str()
                .unwrap_or("version not read"),
            compatibility["layer"].as_str().unwrap_or("-"),
            compatibility["origin"].as_str().unwrap_or("-")
        ));
        let measurements = &adapter["measurements"];
        if measurements.is_null() {
            lines.push("  measurements: no samples".to_owned());
        } else {
            let spans = &measurements["spans"];
            lines.push(format!(
                "  measurements: {} samples; start p50 {} ms, first output p50 {} ms, settle p50 {} ms",
                measurements["samples"],
                span_p50(&spans["start"]),
                span_p50(&spans["first_output"]),
                span_p50(&spans["settle"]),
            ));
        }
        lines.push(String::new());
    }
    lines.join("\n").trim_end().to_owned()
}

fn span_p50(span: &Value) -> String {
    span["p50_ms"]
        .as_u64()
        .map_or_else(|| "n/a".to_owned(), |value| value.to_string())
}

fn push_declaration(lines: &mut Vec<String>, label: &str, declaration: &Value) {
    let value = &declaration["value"];
    let is_no = matches!(value, Value::Bool(false))
        || matches!(value.as_str(), Some("absent" | "unreported"));
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
    use serde_json::json;

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
                    "terminal_event": {"value": "absent", "reason": "process exit is the contract", "source": "row"},
                    "answering_model": {"value": "unreported", "reason": "declared", "source": "row"},
                    "usage_attribution": {"value": "attempt", "reason": "declared", "source": "row"},
                    "live_input": {"value": false, "reason": "no live input", "source": "row"},
                    "session_reference": {"value": "absent", "reason": "raw output carries no session id", "source": "row"},
                    "caveats": []
                },
                "overlay": null,
                "on_gap": {
                    "model": {"action": "warn", "layer": "default", "origin": "policy.LAUNCH_MODEL_UNSUPPORTED", "shadowed": []},
                    "effort": {"action": "warn", "layer": "default", "origin": "policy.LAUNCH_EFFORT_UNSUPPORTED", "shadowed": []},
                    "image_input": {"action": "warn", "layer": "default", "origin": "policy.LAUNCH_IMAGE_INPUT_UNSUPPORTED", "shadowed": []}
                },
                "compatibility": {"status": "version_unread", "version": null, "layer": "default", "origin": null},
                "measurements": null
            }]
        });
        let text = human(&value);
        assert!(text.contains("configured commands take no model selector; source=row"));
        assert!(text.contains("model: no —"));
        assert!(text.contains("effort: yes"));
        assert!(text.contains("compatibility: version_unread"));
        assert!(
            text.contains(
                "on_gap model: warn (layer=default origin=policy.LAUNCH_MODEL_UNSUPPORTED)"
            )
        );
        assert!(text.contains("measurements: no samples"));
    }
}
