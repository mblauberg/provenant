//! `check` — run fast repository checks.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "check",
    verbs: &[
        Verb {
            path: &["check", "config"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["check", "install"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["check", "providers"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["check", "release"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["check", "writing"],
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
        Command::Check(check) => crate::checks::run(check, globals, process.workers()),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
