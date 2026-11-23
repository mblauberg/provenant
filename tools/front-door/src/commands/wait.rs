//! `wait` — wait for graph work or pull requests to settle.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "wait",
    verbs: &[Verb {
        path: &["wait"],
        stability: Stability::Evolving,
        run,
    }],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Wait(cmd) => crate::status_wait::run(cmd, globals),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
