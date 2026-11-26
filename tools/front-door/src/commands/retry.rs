//! `retry` — retry a graph, node or attempt.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "retry",
    verbs: &[Verb {
        path: &["retry"],
        stability: Stability::Evolving,
        run,
    }],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let process = inv.process;
    let command = inv.command.clone();
    let status = match command {
        Command::Retry(cmd) => crate::fabric::unified_graph_command(
            std::slice::from_ref(&cmd.target),
            crate::fabric::GraphCommand::Retry {
                extend: &cmd.extend,
                brief: cmd.brief.as_deref(),
                brief_file: cmd.brief_file.as_deref(),
                queue_control: cmd.queue_control,
                queue_revision: cmd.queue_revision,
                operation_id: cmd.operation_id.as_ref(),
            },
            globals,
            process.workers(),
        ),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
