//! `events` — replay project observations as JSON lines.
use crate::commands::{Domain, Invocation, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "events",
    verbs: &[Verb {
        path: &["events"],
        stability: Stability::Evolving,
        run,
    }],
};

fn run(inv: &Invocation) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Events {
            from,
            follow,
            consumer,
        } => crate::events::run(from.unwrap_or(0), follow, consumer.as_deref(), globals),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
