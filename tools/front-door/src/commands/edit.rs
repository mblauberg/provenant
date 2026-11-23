//! `edit` — apply a patch to unstarted graph nodes.
use crate::commands::{Domain, Invocation, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "edit",
    verbs: &[Verb {
        path: &["edit"],
        stability: Stability::Evolving,
        run,
    }],
};

fn run(inv: &Invocation) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let process = inv.process;
    let command = inv.command.clone();
    let status = match command {
        Command::Edit(cmd) => crate::graph_interface::edit(cmd, globals, process.workers()),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
