//! `build` — inspect and maintain build slots.
use crate::commands::{Domain, Invocation, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "build",
    verbs: &[
        Verb {
            path: &["build", "maintain"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["build", "report"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["build", "reset"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["build", "status"],
            stability: Stability::Evolving,
            run,
        },
    ],
};

fn run(inv: &Invocation) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Build(command) => crate::build_cmd::run(command, globals),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
