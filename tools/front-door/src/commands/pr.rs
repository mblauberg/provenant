//! `pr` — sync, open and inspect pull requests.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "pr",
    verbs: &[
        Verb {
            path: &["pr", "contributions"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["pr", "open"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["pr", "status"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["pr", "sync"],
            stability: Stability::Evolving,
            run,
        },
    ],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let process = inv.process;
    let command = inv.command.clone();
    let status = match command {
        Command::Pr(command) => crate::pr::run(command, globals, process.workers()),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
