//! `accounts` — register and list provider accounts.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, CommandResult, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "accounts",
    verbs: &[
        Verb {
            path: &["accounts", "list"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["accounts", "show"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["accounts", "register"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["accounts", "set"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["accounts", "sync"],
            stability: Stability::Evolving,
            run,
        },
    ],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Accounts(provenant_cli::accounts::Accounts::Sync {
            dry_run,
            adopt,
            force_settings,
            prune_orphans,
        }) => crate::install::run_sync(
            dry_run,
            adopt.as_deref(),
            force_settings,
            prune_orphans,
            globals,
        ),
        Command::Accounts(command) => {
            crate::run_edge(
                inv.leaf,
                globals,
                |edge| match provenant_cli::accounts::execute(&command, edge.roots, edge.home) {
                    Ok(data) => {
                        let listing = matches!(command, provenant_cli::accounts::Accounts::List);
                        let read_only = listing
                            || matches!(command, provenant_cli::accounts::Accounts::Show { .. });
                        let mut reply = provenant_cli::run::Reply {
                            human: data
                                .entries
                                .values()
                                .map(|e| format!("{} {} {}\n", e.adapter, e.label, e.key))
                                .collect(),
                            summary: listing.then(|| crate::accounts_summary(&data)),
                            read_only,
                            result: CommandResult::new(
                                inv.leaf,
                                Outcome::Ok,
                                serde_json::json!(data),
                            ),
                        };
                        match command {
                            provenant_cli::accounts::Accounts::Register {
                                ref adapter,
                                ref label,
                                ..
                            } => {
                                crate::sync_registered_account(globals, &mut reply);
                                crate::probe_registered_account(
                                    edge, &data, adapter, label, &mut reply,
                                );
                            }
                            provenant_cli::accounts::Accounts::List => {
                                crate::report_account_sync(globals, &data, &mut reply);
                            }
                            _ => {}
                        }
                        reply
                    }
                    Err((kind, message)) => {
                        let mut result =
                            CommandResult::new(inv.leaf, Outcome::Refused, serde_json::Value::Null);
                        let projection = kind.map(|kind| kind.diagnostic());
                        let mut error = CommandError::new(
                            kind.map_or("ACCOUNT", |kind| kind.code()),
                            message,
                            None,
                            projection
                                .as_ref()
                                .and_then(|diagnostic| diagnostic.next.as_deref())
                                .unwrap_or("run `provenant accounts sync` and retry"),
                        );
                        error.diagnostic = projection.as_ref().map(Into::into);
                        result.error = Some(error);
                        provenant_cli::run::Reply {
                            result,
                            human: String::new(),
                            summary: None,
                            read_only: false,
                        }
                    }
                },
            )
        }
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
