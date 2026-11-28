//! `output` — read an attempt's output.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "output",
    verbs: &[Verb {
        path: &["output"],
        stability: Stability::Evolving,
        run,
    }],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let process = inv.process;
    let command = inv.command.clone();
    let status = match command {
        Command::Output(cmd) => crate::run_edge(inv.leaf, globals, |edge| {
            crate::fabric::unified_graph_output(
                &cmd,
                edge,
                globals,
                process.workers(),
                std::time::Instant::now()
                    + inv
                        .common
                        .as_ref()
                        .map_or(std::time::Duration::from_secs(30), |common| common.deadline),
            )
        }),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
