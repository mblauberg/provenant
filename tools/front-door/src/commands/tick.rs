//! `tick` — advance due graphs and run scheduled host tasks.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "tick",
    verbs: &[Verb {
        path: &["tick"],
        stability: Stability::Evolving,
        run,
    }],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let process = inv.process;
    let status = match inv.command {
        Command::Tick(args) => crate::tick::run_selected(globals, process.workers(), &args.task),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
