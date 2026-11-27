//! `ci` — read CI evidence for pull requests and Actions runs.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "ci",
    verbs: &[
        Verb {
            path: &["ci", "failures"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["ci", "failures", "pr"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["ci", "failures", "run"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["ci", "patch"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["ci", "patch", "pr"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["ci", "patch", "run"],
            stability: Stability::Evolving,
            run,
        },
    ],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Ci(command) => crate::ci::run(command, globals),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
