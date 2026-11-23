//! `console` — read the local console catalogue.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "console",
    verbs: &[
        Verb {
            path: &["console", "board"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["console", "changes"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["console", "detail"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["console", "open"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["console", "overview"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["console", "projects"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["console", "search"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["console", "sessions"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["console", "tree"],
            stability: Stability::Evolving,
            run,
        },
    ],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Console(cmd) => crate::console::run(cmd, globals),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
