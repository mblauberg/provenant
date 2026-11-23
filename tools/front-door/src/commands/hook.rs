//! `hook` — run harness and git hook handlers.
//!
//! `hook` dispatches before the registry because a provider hook is clap's
//! external subcommand: its name (`hook claude`) is not a grammar leaf. The
//! static `hook git pre-push` leaf is still registered below.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "hook",
    verbs: &[Verb {
        path: &["hook", "git", "pre-push"],
        stability: Stability::Evolving,
        run,
    }],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Hook(hook) => crate::hooks::run(hook, globals),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
