//! `stop` — stop a graph, node or attempt subtree.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "stop",
    verbs: &[Verb {
        path: &["stop"],
        stability: Stability::Evolving,
        run,
    }],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let process = inv.process;
    let command = inv.command.clone();
    let status = match command {
        Command::Stop(cmd) if provenant_cli::fabric::in_lane() => {
            crate::run_edge(inv.leaf, globals, |_| {
                provenant_cli::run::stop_from_lane(&cmd.target)
            })
        }
        Command::Stop(cmd) => crate::fabric::unified_graph_command(
            std::slice::from_ref(&cmd.target),
            crate::fabric::GraphCommand::Stop {
                reason: cmd.reason.as_deref(),
                queue_control: cmd.queue_control,
                queue_revision: cmd.queue_revision,
                operation_id: cmd.operation_id.as_ref(),
            },
            globals,
            process.workers(),
            None,
        ),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
