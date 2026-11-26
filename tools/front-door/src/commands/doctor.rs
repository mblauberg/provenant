//! `doctor` — the top-level host health report, including hook diagnostics.
use crate::commands::{Call, Domain, Verb};
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, Outcome};

pub static DOMAIN: Domain = Domain {
    name: "doctor",
    verbs: &[Verb {
        path: &["doctor"],
        stability: Stability::Evolving,
        run,
    }],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let status = crate::hosts::doctor(None, inv.globals, inv.process.workers(), "doctor");
    crate::commands::rendered(status)
}
