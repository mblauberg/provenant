//! `admit` — dispatched at the process edge before roots and telemetry.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "admit",
    verbs: &[Verb {
        path: &["admit"],
        stability: Stability::Evolving,
        run,
    }],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        // Handled in `dispatch` before roots and telemetry; it never gets here.
        Command::Admit { .. } => crate::fail(
            inv.leaf,
            Outcome::Error,
            "INTERNAL",
            "admit is dispatched before the command table",
            "report this internal dispatch error; rerun the command",
            globals,
        ),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
