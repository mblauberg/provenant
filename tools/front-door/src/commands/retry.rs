//! `retry` — retry a graph, node or attempt.
use crate::commands::{Domain, Invocation, Verb};
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

fn run(inv: &Invocation) -> Result<Outcome, CommandError> {
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
            },
            globals,
            process.workers(),
        ),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
