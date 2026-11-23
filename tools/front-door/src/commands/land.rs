//! `land` — merge on green CI and prune the merged worktree.
use crate::commands::{Domain, Invocation, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "land",
    verbs: &[Verb {
        path: &["land"],
        stability: Stability::Evolving,
        run,
    }],
};

fn run(inv: &Invocation) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let process = inv.process;
    let command = inv.command.clone();
    let status = match command {
        Command::Land {
            numbers,
            strategy,
            target,
            head,
            wait,
            poll,
            explain,
            shadow,
            tier,
            accept_delta,
        } => crate::pr::landing::run(
            crate::pr::landing::Options {
                numbers,
                strategy,
                target,
                head,
                wait,
                poll,
                explain,
                shadow,
                tier,
                accept_delta,
            },
            globals,
            process.workers(),
        ),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
