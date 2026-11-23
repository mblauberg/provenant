//! `trace` — show a graph's recorded revisions and attempt timeline.
use crate::commands::{Domain, Invocation, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "trace",
    verbs: &[Verb {
        path: &["trace"],
        stability: Stability::Evolving,
        run,
    }],
};

fn run(inv: &Invocation) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let process = inv.process;
    let command = inv.command.clone();
    let status = match command {
        Command::Trace(cmd) => {
            crate::fabric::unified_graph_trace(&cmd.target, globals, process.workers())
        }
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
