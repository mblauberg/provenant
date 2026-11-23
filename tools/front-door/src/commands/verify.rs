//! `verify` — run a declared repository verification tier.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "verify",
    verbs: &[Verb {
        path: &["verify"],
        stability: Stability::Evolving,
        run,
    }],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Verify {
            tier,
            commit,
            affected,
        } => crate::verify::run(tier.as_deref(), commit.as_deref(), affected, globals),
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}
