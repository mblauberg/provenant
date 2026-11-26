//! `handoff` — declare a lane's result through the host-bound journal.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "handoff",
    verbs: &[Verb {
        path: &["handoff"],
        stability: Stability::Evolving,
        run,
    }],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let command = inv.command.clone();
    let status = match command {
        Command::Handoff(cmd) => provenant_cli::declaration::run(cmd, inv.globals),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
