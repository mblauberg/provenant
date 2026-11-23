//! `help` — resolved by the parser, never by a verb body.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "help",
    verbs: &[Verb {
        path: &["help"],
        stability: Stability::Evolving,
        run,
    }],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Help { .. } => crate::fail(
            "help",
            Outcome::Invalid,
            "INVALID",
            "help must be resolved by the parser",
            "report this internal parser error; rerun `provenant help`",
            globals,
        ),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
