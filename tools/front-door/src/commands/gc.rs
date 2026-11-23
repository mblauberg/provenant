//! `gc` — collect lane caches, ignored output, merged worktrees and captures.
use crate::commands::{Domain, Invocation, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "gc",
    verbs: &[Verb {
        path: &["gc"],
        stability: Stability::Evolving,
        run,
    }],
};

fn run(inv: &Invocation) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let process = inv.process;
    let command = inv.command.clone();
    let status = match command {
        Command::Gc(command) => crate::worktrees::run_gc(command, globals, process.workers()),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
