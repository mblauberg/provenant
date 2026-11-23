//! `native` — record metadata for a native sub-agent attempt.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "native",
    verbs: &[
        Verb {
            path: &["native", "end"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["native", "start"],
            stability: Stability::Evolving,
            run,
        },
    ],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Native(cmd) => crate::usage::native(cmd, globals),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
