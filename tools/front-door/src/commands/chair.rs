//! `chair` — manage the chair session.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "chair",
    verbs: &[
        Verb {
            path: &["chair", "log", "append"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["chair", "log", "list"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["chair", "message"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["chair", "summary", "diff"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["chair", "summary", "show"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["chair", "summary", "update"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["chair", "switch"],
            stability: Stability::Evolving,
            run,
        },
    ],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Chair(provenant_cli::Chair::Message(args)) => crate::human_inbox(
            provenant_cli::inbox::HumanCommand::ChairMessage(args),
            inv.leaf,
            globals,
        ),
        Command::Chair(cmd) => crate::chair::run(cmd, globals),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
