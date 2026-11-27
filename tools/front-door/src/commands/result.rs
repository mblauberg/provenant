//! `result` — summarise a lane's handoff, branch, head and verification.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "result",
    verbs: &[Verb {
        path: &["result"],
        stability: Stability::Evolving,
        run,
    }],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Result { target, .. } => crate::run_edge(inv.leaf, globals, |edge| {
            crate::fabric::unified_graph_result(&target, edge)
        }),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
