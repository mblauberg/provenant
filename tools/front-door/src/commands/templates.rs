//! `templates` — list or show templates from all layers.
use crate::commands::{Domain, Invocation, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "templates",
    verbs: &[
        Verb {
            path: &["templates"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["templates", "show"],
            stability: Stability::Evolving,
            run,
        },
    ],
};

fn run(inv: &Invocation) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let process = inv.process;
    let command = inv.command.clone();
    let status = match command {
        Command::Templates(cmd) => {
            crate::graph_interface::templates(cmd, globals, process.workers())
        }
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
