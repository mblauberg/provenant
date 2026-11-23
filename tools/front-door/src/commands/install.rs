//! `install`, `upgrade` and `uninstall` — the host installation lifecycle.
use crate::commands::{Domain, Invocation, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "install",
    verbs: &[
        Verb {
            path: &["install"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["uninstall"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["upgrade"],
            stability: Stability::Evolving,
            run,
        },
    ],
};

fn run(inv: &Invocation) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Install(args) => crate::install::run_install(args, globals),
        Command::Upgrade(args) => crate::install::run_upgrade(args, globals),
        Command::Uninstall(args) => crate::install::run_uninstall(args, globals),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
