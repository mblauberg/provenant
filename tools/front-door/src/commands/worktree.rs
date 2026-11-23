//! `worktree` — create, list and remove worktrees.
use crate::commands::{Domain, Invocation, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "worktree",
    verbs: &[
        Verb {
            path: &["worktree", "create"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["worktree", "doctor"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["worktree", "list"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["worktree", "remove"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["worktree", "session-list"],
            stability: Stability::Evolving,
            run,
        },
    ],
};

fn run(inv: &Invocation) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let process = inv.process;
    let command = inv.command.clone();
    let status = match command {
        Command::Worktree(worktree) => crate::worktrees::run(worktree, globals, process.workers()),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
