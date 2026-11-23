//! `adopt` and `release` — take an item into custody and release it.
use crate::commands::{Domain, Invocation, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "custody",
    verbs: &[
        Verb {
            path: &["adopt"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["release"],
            stability: Stability::Evolving,
            run,
        },
    ],
};

fn run(inv: &Invocation) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Adopt { subject } => crate::custody::command(globals, subject, false),
        Command::Release { subject } => crate::custody::command(globals, subject, true),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
