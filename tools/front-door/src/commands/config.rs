//! `config` — show, explain and edit configuration.
use crate::commands::{Call, Domain, Verb};
use crate::roots::{Product, Workspace};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, CommandResult, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "config",
    verbs: &[
        Verb {
            path: &["config", "apply"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["config", "check"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["config", "detect"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["config", "explain"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["config", "history"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["config", "path"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["config", "ports"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["config", "schema"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["config", "set"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["config", "show"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["config", "unset"],
            stability: Stability::Evolving,
            run,
        },
    ],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    if let Command::Config(provenant_cli::Config::Explain { key }) = &command {
        return explain(key, globals);
    }
    let status = match command {
        Command::Config(provenant_cli::Config::Ports) => {
            crate::config_command("config ports", crate::ConfigAction::Ports, globals)
        }
        Command::Config(provenant_cli::Config::Detect) => {
            crate::config_command("config detect", crate::ConfigAction::Detect, globals)
        }
        Command::Config(provenant_cli::Config::Path { dir }) => crate::config_command(
            "config path",
            crate::ConfigAction::Path {
                dir: dir.as_deref(),
            },
            globals,
        ),
        Command::Config(provenant_cli::Config::Check(page)) => crate::checks::config(page, globals),
        Command::Config(provenant_cli::Config::Show) => {
            crate::config_command("config show", crate::ConfigAction::Show, globals)
        }
        Command::Config(provenant_cli::Config::Schema { all }) => crate::config_command(
            "config schema",
            crate::ConfigAction::Schema { all },
            globals,
        ),
        Command::Config(provenant_cli::Config::Apply {
            file,
            expect_revision,
            dry_run,
        }) => crate::config_command(
            "config apply",
            crate::ConfigAction::Apply {
                patch: &file,
                revision: &expect_revision,
                dry_run,
            },
            globals,
        ),
        Command::Config(provenant_cli::Config::History { since, limit }) => crate::config_command(
            "config history",
            crate::ConfigAction::History {
                since,
                limit: limit as usize,
            },
            globals,
        ),
        Command::Config(provenant_cli::Config::Set { name, value }) => crate::config_command(
            "config set",
            crate::ConfigAction::Edit(if name == "profile" {
                provenant_config::KnobEdit::Profile(&value)
            } else {
                provenant_config::KnobEdit::Set {
                    name: &name,
                    value: &value,
                }
            }),
            globals,
        ),
        Command::Config(provenant_cli::Config::Unset { name }) => crate::config_command(
            "config unset",
            crate::ConfigAction::Edit(provenant_config::KnobEdit::Unset(&name)),
            globals,
        ),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}

/// One candidate as a short human line: value, layer, exact origin and the
/// specificity that decided ties inside a layer.
fn render_candidate(candidate: &serde_json::Value) -> String {
    let value = candidate
        .get("value")
        .map(ToString::to_string)
        .unwrap_or_else(|| "null".to_owned());
    let layer = candidate
        .get("layer")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("default");
    let origin = candidate
        .get("origin")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unspecified");
    let specificity = candidate
        .get("specificity")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    format!("{value} — {layer} ({origin}, specificity {specificity})")
}

fn render_explanation(value: &serde_json::Value) -> String {
    let key = value
        .get("key")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("setting");
    let mut out = format!("{key}\n");
    if let Some(winner) = value.get("winner").filter(|candidate| !candidate.is_null()) {
        out.push_str(&format!("  winner: {}\n", render_candidate(winner)));
    }
    for (label, field) in [("shadowed", "shadowed"), ("ignored", "ignored")] {
        if let Some(candidates) = value.get(field).and_then(serde_json::Value::as_array) {
            for candidate in candidates {
                out.push_str(&format!("  {label}: {}\n", render_candidate(candidate)));
            }
        }
    }
    out.push_str("Use `--json` for the full candidate list.\n");
    out
}

/// `config explain KEY`: the winner and every candidate it beat, with the
/// layer, origin and specificity that resolved the setting.
fn explain(key: &str, globals: &provenant_cli::Globals) -> Result<Outcome, CommandError> {
    let result = (|| -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let context = crate::roots::resolve(globals, Workspace::Project, Product::Required)?;
        let explanation = provenant_config::explain(key, &context.roots, Some(&context.project))?;
        Ok(serde_json::to_value(explanation)?)
    })();
    let status = match result {
        Ok(value) => {
            let human = render_explanation(&value);
            crate::emit(
                &CommandResult::new("config explain", Outcome::Ok, value),
                globals,
                &human,
                true,
            )
        }
        Err(error) => crate::fail(
            "config explain",
            Outcome::Invalid,
            "CONFIG_UNKNOWN_KEY",
            &error.to_string(),
            "run `provenant config schema --all` and use a declared key",
            globals,
        ),
    };
    crate::commands::rendered(status)
}

#[cfg(test)]
mod explanation_tests {
    #[test]
    fn explanation_human_output_names_every_candidate() {
        let value = serde_json::json!({
            "key": "policy.CODE",
            "winner": {"value": "off", "layer": "launch", "origin": "--checks", "specificity": 1},
            "shadowed": [
                {"value": "refuse", "layer": "host", "origin": "config.toml#policy.CODE", "specificity": 3}
            ],
            "ignored": []
        });
        let human = super::render_explanation(&value);
        assert!(human.starts_with("policy.CODE\n"));
        assert!(human.contains("winner: off — launch (--checks, specificity 1)"));
        assert!(human.contains("shadowed: refuse — host (config.toml#policy.CODE, specificity 3)"));
        assert!(!human.contains("ignored:"));
    }
}
