//! `config` — show and edit configuration.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

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
    if matches!(
        &command,
        Command::Config(provenant_cli::Config::Explain { .. })
    ) {
        return crate::commands::not_implemented(inv.leaf);
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
