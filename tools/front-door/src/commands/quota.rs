//! `quota` — show and refresh provider quota.
use crate::commands::{Domain, Invocation, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "quota",
    verbs: &[
        Verb {
            path: &["quota"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["quota", "clear"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["quota", "display"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["quota", "history"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["quota", "show"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["quota", "tick"],
            stability: Stability::Evolving,
            run,
        },
    ],
};

fn run(inv: &Invocation) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Quota { command } => {
            let cmd = command.unwrap_or(provenant_cli::Quota::Show(provenant_cli::quota::Filter {
                adapter: None,
            }));
            match crate::roots::resolve(
                globals,
                crate::roots::Workspace::Project,
                crate::roots::Product::Required,
            ) {
                Ok(resolved) => match crate::live_quota(&resolved) {
                    Ok(effects) => {
                        let housekeeping = matches!(cmd, provenant_cli::Quota::Tick { .. });
                        let result = provenant_cli::quota::run(
                            cmd,
                            globals,
                            &resolved.roots,
                            provenant_providers::REGISTRY,
                            &effects,
                            |result| crate::overflow::publish(result, globals),
                        );
                        if housekeeping {
                            let _ = provenant_cli::provider_updates::schedule(
                                &resolved.roots,
                                &resolved.project,
                            );
                        }
                        result
                    }
                    Err(_) => crate::fail(
                        inv.leaf,
                        Outcome::Invalid,
                        "QUOTA_CONFIG",
                        "invalid quota accounts configuration",
                        "run `provenant check config --json` and correct the accounts configuration",
                        globals,
                    ),
                },
                Err(error) => crate::fail(
                    inv.leaf,
                    Outcome::Error,
                    "ROOT",
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
