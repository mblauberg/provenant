//! `hosts` — list hosts and check remote sign-in.
use crate::commands::{Domain, Invocation, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "hosts",
    verbs: &[
        Verb {
            path: &["hosts"],
            stability: Stability::Evolving,
            run,
        },
        Verb {
            path: &["hosts", "doctor"],
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
        Command::Hosts { command: None } => crate::fabric::hosts(globals, process.workers()),
        Command::Hosts {
            command: Some(command),
        } => crate::hosts::run(command, globals, process.workers()),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
