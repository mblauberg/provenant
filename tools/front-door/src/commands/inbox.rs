//! `ask`, `answer` and `inbox` — human decisions, assumptions and questions.
use crate::commands::{Domain, Invocation, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "inbox",
    verbs: &[
        Verb {
            path: &["answer"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["ask"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["inbox"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["inbox", "acknowledge"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["inbox", "close"],
            stability: Stability::Evolving,
            run,
        },
    ],
};

fn run(inv: &Invocation) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Ask(args) => crate::human_inbox(
            provenant_cli::inbox::HumanCommand::Ask(args),
            inv.leaf,
            globals,
        ),
        Command::Inbox(args) => crate::human_inbox(
            provenant_cli::inbox::HumanCommand::Inbox(*args),
            inv.leaf,
            globals,
        ),
        Command::Answer(args) => crate::human_inbox(
            provenant_cli::inbox::HumanCommand::Answer(*args),
            inv.leaf,
            globals,
        ),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
