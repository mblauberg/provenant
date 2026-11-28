//! `status` — show graph, node and attempt status.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "status",
    verbs: &[Verb {
        path: &["status"],
        stability: Stability::Evolving,
        run,
    }],
};

#[allow(
    clippy::disallowed_methods,
    reason = "renders command output to stdout for the caller"
)]
fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let process = inv.process;
    let command = inv.command.clone();
    let deadline = crate::fabric::invocation_deadline::after(
        inv.common
            .as_ref()
            .map_or(std::time::Duration::from_secs(30), |common| common.deadline),
    );
    let status = match command {
        Command::Status(cmd) => {
            if cmd.session_stream {
                crate::run_edge(inv.leaf, globals, |edge| {
                    provenant_cli::run::status_session_stream(
                        cmd,
                        edge,
                        deadline,
                        &mut std::io::stdout().lock(),
                    )
                })
            } else if cmd.mine || cmd.adoptable {
                crate::custody::status(&cmd, globals)
            } else if cmd.targets.is_empty() && !provenant_cli::fabric::in_lane() {
                crate::fabric::graph_listing(&cmd, globals, process.workers(), deadline)
            } else {
                crate::fabric::unified_graph_command(
                    &cmd.targets,
                    crate::fabric::GraphCommand::Status,
                    globals,
                    process.workers(),
                    Some(deadline),
                )
            }
        }
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
