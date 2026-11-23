//! `notes` — share append-only observations with project lanes.
use crate::commands::{Domain, Invocation, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "notes",
    verbs: &[
        Verb {
            path: &["notes", "add"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["notes", "read"],
            stability: Stability::Evolving,
            run,
        },
    ],
};

fn run(inv: &Invocation) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Notes(cmd) => crate::notes::run(cmd, globals),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
