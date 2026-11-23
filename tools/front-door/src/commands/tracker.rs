//! `tracker` — inspect issue specifications.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "tracker",
    verbs: &[Verb {
        path: &["tracker", "lint"],
        stability: Stability::Evolving,
        run,
    }],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Tracker(command) => crate::tracker::run(command, globals),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
