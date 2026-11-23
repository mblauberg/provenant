//! `disk` — inspect disk use without changing files.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "disk",
    verbs: &[Verb {
        path: &["disk", "report"],
        stability: Stability::Evolving,
        run,
    }],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Disk(provenant_cli::Disk::Report) => crate::disk::run(globals),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
