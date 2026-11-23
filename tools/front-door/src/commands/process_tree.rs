//! `process-tree` — read a fresh resident-memory sample of an owned lane tree.
use crate::commands::{Domain, Invocation, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "process-tree",
    verbs: &[Verb {
        path: &["process-tree"],
        stability: Stability::Stable,
        run,
    }],
};

fn run(inv: &Invocation) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::ProcessTree { root } => crate::process_tree::run(root, globals),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
