//! `lane` — deliver a settled writer attempt.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "lane",
    verbs: &[Verb {
        path: &["lane", "deliver"],
        stability: Stability::Evolving,
        run,
    }],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let process = inv.process;
    let command = inv.command.clone();
    let status = match command {
        Command::Lane(cmd) => crate::run_edge(inv.leaf, globals, |edge| {
            let mut reply = provenant_cli::run::lane(cmd, edge);
            if let Err(error) =
                crate::fabric::settle_delivered_attempt(edge, &mut reply, process.workers())
            {
                reply
                    .human
                    .push_str(&format!("worktree retained: {error}\n"));
                reply.result.data["worktree_settlement_error"] =
                    serde_json::json!(error.to_string());
            }
            reply
        }),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
